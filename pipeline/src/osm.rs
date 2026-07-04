use crate::config::{City, FeatureType};
use anyhow::{bail, Context, Result};
use osmpbf::{Blob, BlobDecode, BlobReader};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[derive(Serialize, Deserialize, Clone, Copy)]
pub struct Segment {
    pub a: u32,
    pub b: u32,
    /// highway=steps
    pub steps: bool,
    /// bridge or tunnel: ignore DEM slope (the DEM sees the terrain, not the deck)
    pub flat: bool,
}

#[derive(Serialize, Deserialize)]
pub struct Building {
    /// outer ring, lng/lat, closed
    pub ring: Vec<[f64; 2]>,
    pub centroid: [f64; 2],
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Feat {
    pub ll: [f64; 2],
    pub name: Option<String>,
}

/// Everything extracted from one city's PBF.
#[derive(Serialize, Deserialize)]
pub struct CityData {
    /// graph node coordinates (lng, lat) — every node of every walkable way
    pub node_ll: Vec<[f64; 2]>,
    pub segments: Vec<Segment>,
    pub buildings: Vec<Building>,
    /// per feature type (same order as config)
    pub features: Vec<Vec<Feat>>,
}

/// Walkability filter. Returns (steps, flat) flags if the way is walkable.
pub fn walkable(tags: &HashMap<&str, &str>) -> Option<(bool, bool)> {
    let highway = *tags.get("highway")?;
    const OK: &[&str] = &[
        "footway",
        "path",
        "pedestrian",
        "steps",
        "living_street",
        "residential",
        "service",
        "track",
        "unclassified",
        "tertiary",
        "tertiary_link",
        "secondary",
        "secondary_link",
        "primary",
        "primary_link",
        "road",
        "bridleway",
        "corridor",
        "cycleway",
    ];
    let foot = tags.get("foot").copied();
    let sidewalk = tags.get("sidewalk").copied();
    let foot_yes = matches!(foot, Some("yes" | "designated" | "permissive"));
    if !OK.contains(&highway) {
        // trunk roads only with an explicit sidewalk / foot permission
        let trunk = highway == "trunk" || highway == "trunk_link";
        let has_sidewalk = matches!(sidewalk, Some("yes" | "left" | "right" | "both"));
        if !(trunk && (has_sidewalk || foot_yes)) {
            return None;
        }
    }
    if matches!(foot, Some("no" | "private" | "use_sidepath" | "discouraged")) {
        return None;
    }
    if matches!(tags.get("access").copied(), Some("no" | "private" | "military")) && !foot_yes {
        return None;
    }
    if tags.contains_key("ferry") || tags.get("route").copied() == Some("ferry") {
        return None;
    }
    if tags.get("service").copied() == Some("drive-through") {
        return None;
    }
    let steps = highway == "steps";
    let not_no = |k: &str| matches!(tags.get(k), Some(&v) if v != "no");
    let flat = not_no("bridge") || not_no("tunnel");
    Some((steps, flat))
}

fn is_building(tags: &HashMap<&str, &str>) -> bool {
    matches!(tags.get("building"), Some(&v) if v != "no")
}

struct WalkWay {
    refs: Vec<i64>,
    steps: bool,
    flat: bool,
}

struct FeatWay {
    refs: Vec<i64>,
    mask: u32,
    name: Option<String>,
}

#[derive(Default)]
struct Pass1 {
    walk: Vec<WalkWay>,
    blds: Vec<Vec<i64>>,
    feats: Vec<FeatWay>,
}

struct FeatNode {
    ll: [f64; 2],
    mask: u32,
    name: Option<String>,
}

#[derive(Default)]
struct Pass2 {
    coords: Vec<(i64, [f64; 2])>,
    feats: Vec<FeatNode>,
}

fn in_bbox(bbox: Option<[f64; 4]>, lng: f64, lat: f64) -> bool {
    match bbox {
        None => true,
        Some([w, s, e, n]) => lng >= w && lng <= e && lat >= s && lat <= n,
    }
}

/// tag-only feature type mask (location filters applied later, once coords are known)
fn type_mask(types: &[FeatureType], tags: &HashMap<&str, &str>) -> u32 {
    let mut mask = 0u32;
    for (i, t) in types.iter().enumerate() {
        if t.r#match.matches(tags) {
            mask |= 1 << i;
        }
    }
    mask
}

fn ring_centroid(ring: &[[f64; 2]]) -> [f64; 2] {
    // area-weighted centroid; falls back to vertex mean for degenerate rings
    let n = ring.len();
    let (mut a2, mut cx, mut cy) = (0.0, 0.0, 0.0);
    for i in 0..n {
        let p = ring[i];
        let q = ring[(i + 1) % n];
        let cross = p[0] * q[1] - q[0] * p[1];
        a2 += cross;
        cx += (p[0] + q[0]) * cross;
        cy += (p[1] + q[1]) * cross;
    }
    if a2.abs() > 1e-14 {
        [cx / (3.0 * a2), cy / (3.0 * a2)]
    } else {
        let (sx, sy) = ring.iter().fold((0.0, 0.0), |(x, y), p| (x + p[0], y + p[1]));
        [sx / n as f64, sy / n as f64]
    }
}

pub fn extract(pbf: &Path, city: &City, types: &[FeatureType]) -> Result<CityData> {
    // ---- pass 1: ways ----
    let p1 = BlobReader::from_path(pbf)
        .with_context(|| format!("opening {}", pbf.display()))?
        .par_bridge()
        .map(|blob| -> Result<Pass1> {
            let mut acc = Pass1::default();
            let blob: Blob = blob?;
            if let BlobDecode::OsmData(block) = blob.decode()? {
                for group in block.groups() {
                    for way in group.ways() {
                        let tags: HashMap<&str, &str> = way.tags().collect();
                        if tags.is_empty() {
                            continue;
                        }
                        if let Some((steps, flat)) = walkable(&tags) {
                            acc.walk.push(WalkWay { refs: way.refs().collect(), steps, flat });
                        }
                        if is_building(&tags) {
                            acc.blds.push(way.refs().collect());
                        }
                        let mask = type_mask(types, &tags);
                        if mask != 0 {
                            acc.feats.push(FeatWay {
                                refs: way.refs().collect(),
                                mask,
                                name: tags.get("name").map(|s| s.to_string()),
                            });
                        }
                    }
                }
            }
            Ok(acc)
        })
        .try_reduce(Pass1::default, |mut a, b| {
            a.walk.extend(b.walk);
            a.blds.extend(b.blds);
            a.feats.extend(b.feats);
            Ok(a)
        })?;

    eprintln!(
        "  pass1: {} walkable ways, {} buildings, {} feature ways",
        p1.walk.len(),
        p1.blds.len(),
        p1.feats.len()
    );

    let mut needed: FxHashSet<i64> = FxHashSet::default();
    for w in &p1.walk {
        needed.extend(&w.refs);
    }
    for b in &p1.blds {
        needed.extend(b);
    }
    for f in &p1.feats {
        needed.extend(&f.refs);
    }

    // ---- pass 2: nodes ----
    let bbox = city.bbox;
    let p2 = BlobReader::from_path(pbf)?
        .par_bridge()
        .map(|blob| -> Result<Pass2> {
            let mut acc = Pass2::default();
            let blob: Blob = blob?;
            if let BlobDecode::OsmData(block) = blob.decode()? {
                for group in block.groups() {
                    for node in group.dense_nodes() {
                        let (lng, lat) = (node.lon(), node.lat());
                        if !in_bbox(bbox, lng, lat) {
                            continue;
                        }
                        if needed.contains(&node.id()) {
                            acc.coords.push((node.id(), [lng, lat]));
                        }
                        let mut tags = node.tags().peekable();
                        if tags.peek().is_some() {
                            let map: HashMap<&str, &str> = tags.collect();
                            let mut mask = 0u32;
                            for (i, t) in types.iter().enumerate() {
                                if t.matches(&map, lng, lat) {
                                    mask |= 1 << i;
                                }
                            }
                            if mask != 0 {
                                acc.feats.push(FeatNode {
                                    ll: [lng, lat],
                                    mask,
                                    name: map.get("name").map(|s| s.to_string()),
                                });
                            }
                        }
                    }
                    for node in group.nodes() {
                        let (lng, lat) = (node.lon(), node.lat());
                        if !in_bbox(bbox, lng, lat) {
                            continue;
                        }
                        if needed.contains(&node.id()) {
                            acc.coords.push((node.id(), [lng, lat]));
                        }
                        let map: HashMap<&str, &str> = node.tags().collect();
                        if !map.is_empty() {
                            let mut mask = 0u32;
                            for (i, t) in types.iter().enumerate() {
                                if t.matches(&map, lng, lat) {
                                    mask |= 1 << i;
                                }
                            }
                            if mask != 0 {
                                acc.feats.push(FeatNode {
                                    ll: [lng, lat],
                                    mask,
                                    name: map.get("name").map(|s| s.to_string()),
                                });
                            }
                        }
                    }
                }
            }
            Ok(acc)
        })
        .try_reduce(Pass2::default, |mut a, b| {
            a.coords.extend(b.coords);
            a.feats.extend(b.feats);
            Ok(a)
        })?;
    drop(needed);

    let coords: FxHashMap<i64, [f64; 2]> = p2.coords.into_iter().collect();
    eprintln!("  pass2: {} node coords, {} feature nodes", coords.len(), p2.feats.len());
    if coords.is_empty() {
        bail!("no nodes found — is the bbox inside the PBF extent?");
    }

    // ---- assemble graph ----
    let mut compact: FxHashMap<i64, u32> = FxHashMap::default();
    let mut node_ll: Vec<[f64; 2]> = Vec::new();
    let mut segments: Vec<Segment> = Vec::new();
    for w in &p1.walk {
        for pair in w.refs.windows(2) {
            let (Some(&la), Some(&lb)) = (coords.get(&pair[0]), coords.get(&pair[1])) else {
                continue; // clipped at bbox edge
            };
            let a = *compact.entry(pair[0]).or_insert_with(|| {
                node_ll.push(la);
                (node_ll.len() - 1) as u32
            });
            let b = *compact.entry(pair[1]).or_insert_with(|| {
                node_ll.push(lb);
                (node_ll.len() - 1) as u32
            });
            if a != b {
                segments.push(Segment { a, b, steps: w.steps, flat: w.flat });
            }
        }
    }
    drop(compact);

    // ---- buildings ----
    let buildings: Vec<Building> = p1
        .blds
        .par_iter()
        .filter_map(|refs| {
            let mut ring: Vec<[f64; 2]> = Vec::with_capacity(refs.len());
            for r in refs {
                ring.push(*coords.get(r)?); // any missing node → skip building
            }
            if ring.len() < 4 {
                return None;
            }
            let centroid = ring_centroid(&ring);
            in_bbox(bbox, centroid[0], centroid[1]).then_some(Building { ring, centroid })
        })
        .collect();

    // ---- features per type ----
    let mut features: Vec<Vec<Feat>> = vec![Vec::new(); types.len()];
    for fnode in &p2.feats {
        for (i, _) in types.iter().enumerate() {
            if fnode.mask & (1 << i) != 0 {
                features[i].push(Feat { ll: fnode.ll, name: fnode.name.clone() });
            }
        }
    }
    for fway in &p1.feats {
        let pts: Vec<[f64; 2]> = fway.refs.iter().filter_map(|r| coords.get(r).copied()).collect();
        if pts.is_empty() {
            continue;
        }
        let c = ring_centroid(&pts);
        if !in_bbox(bbox, c[0], c[1]) {
            continue;
        }
        for (i, t) in types.iter().enumerate() {
            if fway.mask & (1 << i) != 0 {
                // re-check the location clause now that we know where it is
                if t.within.map_or(true, |[w, s, e, n]| {
                    c[0] >= w && c[0] <= e && c[1] >= s && c[1] <= n
                }) {
                    features[i].push(Feat { ll: c, name: fway.name.clone() });
                }
            }
        }
    }

    eprintln!(
        "  graph: {} nodes, {} segments; {} buildings; features: {}",
        node_ll.len(),
        segments.len(),
        buildings.len(),
        features.iter().map(|f| f.len().to_string()).collect::<Vec<_>>().join("/")
    );

    Ok(CityData { node_ll, segments, buildings, features })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t<'a>(kv: &[(&'a str, &'a str)]) -> HashMap<&'a str, &'a str> {
        kv.iter().copied().collect()
    }

    #[test]
    fn walkability() {
        assert!(walkable(&t(&[("highway", "footway")])).is_some());
        assert!(walkable(&t(&[("highway", "residential")])).is_some());
        assert!(walkable(&t(&[("highway", "motorway")])).is_none());
        assert!(walkable(&t(&[("highway", "trunk")])).is_none());
        assert!(walkable(&t(&[("highway", "trunk"), ("sidewalk", "both")])).is_some());
        assert!(walkable(&t(&[("highway", "footway"), ("foot", "no")])).is_none());
        assert!(walkable(&t(&[("highway", "path"), ("access", "private")])).is_none());
        assert!(walkable(&t(&[("highway", "path"), ("access", "private"), ("foot", "yes")])).is_some());
        assert!(walkable(&t(&[("highway", "footway"), ("route", "ferry")])).is_none());
        let (steps, _) = walkable(&t(&[("highway", "steps")])).unwrap();
        assert!(steps);
        let (_, flat) = walkable(&t(&[("highway", "footway"), ("bridge", "yes")])).unwrap();
        assert!(flat);
        assert!(!walkable(&t(&[("highway", "footway"), ("bridge", "no")])).unwrap().1);
    }
}
