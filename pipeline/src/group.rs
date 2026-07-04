//! Group near-duplicate features (e.g. the two directional bus stops of one
//! named stop) into single sites: within 100 m AND similar normalized names.

use crate::osm::Feat;
use crate::weights::haversine_m;

pub struct SiteGroup {
    pub name: Option<String>,
    /// centroid of the members (used for search fly-to and the site dot)
    pub ll: [f64; 2],
    /// every member's own location — each gets seeded in the Dijkstra
    pub member_lls: Vec<[f64; 2]>,
}

pub const GROUP_RADIUS_M: f64 = 100.0;

fn normalize(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn similar(a: &str, b: &str) -> bool {
    let (a, b) = (normalize(a), normalize(b));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a == b || a.starts_with(&b) || b.starts_with(&a)
}

struct Dsu(Vec<usize>);
impl Dsu {
    fn find(&mut self, mut x: usize) -> usize {
        while self.0[x] != x {
            self.0[x] = self.0[self.0[x]];
            x = self.0[x];
        }
        x
    }
    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.0[ra] = rb;
        }
    }
}

pub fn group_features(feats: &[Feat]) -> Vec<SiteGroup> {
    let n = feats.len();
    let mut dsu = Dsu((0..n).collect());
    // simple lat-sorted sweep keeps this O(n·k) — fine at city scale
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| feats[a].ll[1].partial_cmp(&feats[b].ll[1]).unwrap());
    let lat_window = GROUP_RADIUS_M / 110_574.0;
    for (oi, &i) in order.iter().enumerate() {
        let Some(name_i) = &feats[i].name else { continue };
        for &j in order[oi + 1..].iter() {
            if feats[j].ll[1] - feats[i].ll[1] > lat_window {
                break;
            }
            let Some(name_j) = &feats[j].name else { continue };
            if similar(name_i, name_j) && haversine_m(feats[i].ll, feats[j].ll) <= GROUP_RADIUS_M {
                dsu.union(i, j);
            }
        }
    }
    let mut groups: rustc_hash::FxHashMap<usize, Vec<usize>> = rustc_hash::FxHashMap::default();
    for i in 0..n {
        groups.entry(dsu.find(i)).or_default().push(i);
    }
    let mut out: Vec<SiteGroup> = groups
        .into_values()
        .map(|members| {
            let k = members.len() as f64;
            let (sx, sy) = members
                .iter()
                .fold((0.0, 0.0), |(x, y), &m| (x + feats[m].ll[0], y + feats[m].ll[1]));
            SiteGroup {
                name: feats[members[0]].name.clone(),
                ll: [sx / k, sy / k],
                member_lls: members.iter().map(|&m| feats[m].ll).collect(),
            }
        })
        .collect();
    // deterministic order (west→east, then south→north)
    out.sort_by(|a, b| a.ll.partial_cmp(&b.ll).unwrap());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(name: Option<&str>, lng: f64, lat: f64) -> Feat {
        Feat { ll: [lng, lat], name: name.map(|s| s.to_string()) }
    }

    #[test]
    fn name_similarity() {
        assert!(similar("Fonte da Pedra", "fonte da pedra"));
        assert!(similar("Fonte da Pedra", "Fonte da Pedra (S)")); // prefix
        assert!(!similar("Fonte da Pedra", "Ribeiro Seco"));
        assert!(!similar("", "x"));
    }

    #[test]
    fn directional_stops_merge() {
        // two same-name stops ~60m apart + one unrelated
        let feats = vec![
            f(Some("Fonte da Pedra"), -16.93, 32.65),
            f(Some("Fonte da Pedra"), -16.93, 32.6505), // ~55m north
            f(Some("Hospital"), -16.94, 32.65),
        ];
        let groups = group_features(&feats);
        assert_eq!(groups.len(), 2);
        let g = groups.iter().find(|g| g.name.as_deref() == Some("Fonte da Pedra")).unwrap();
        assert_eq!(g.member_lls.len(), 2);
        assert!((g.ll[1] - 32.65025).abs() < 1e-6, "centroid latitude");
    }

    #[test]
    fn same_name_far_apart_stay_separate() {
        // "High Street" stops in different neighbourhoods (~2km) don't merge
        let feats = vec![f(Some("High Street"), -0.1, 51.5), f(Some("High Street"), -0.1, 51.52)];
        assert_eq!(group_features(&feats).len(), 2);
    }

    #[test]
    fn unnamed_never_merge() {
        let feats = vec![f(None, 0.0, 0.0), f(None, 0.0, 0.00001)];
        assert_eq!(group_features(&feats).len(), 2);
    }
}
