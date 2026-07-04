use kiddo::{KdTree, SquaredEuclidean};

/// Nearest-graph-node lookup in locally-projected meters
/// (equirectangular around the city's mean latitude — fine at city scale).
pub struct Snapper {
    tree: KdTree<f64, 2>,
    kx: f64,
    ky: f64,
    len: usize,
}

pub const KY: f64 = 110_574.0;

impl Snapper {
    pub fn new(node_ll: &[[f64; 2]], lat0: f64) -> Snapper {
        let kx = 111_320.0 * lat0.to_radians().cos();
        let mut tree: KdTree<f64, 2> = KdTree::with_capacity(node_ll.len());
        for (i, p) in node_ll.iter().enumerate() {
            tree.add(&[p[0] * kx, p[1] * KY], i as u64);
        }
        Snapper { tree, kx, ky: KY, len: node_ll.len() }
    }

    /// Nearest node within `max_m` meters: (node index, distance in meters).
    pub fn nearest(&self, ll: [f64; 2], max_m: f64) -> Option<(u32, f64)> {
        if self.len == 0 {
            return None;
        }
        let nn = self.tree.nearest_one::<SquaredEuclidean>(&[ll[0] * self.kx, ll[1] * self.ky]);
        let d = nn.distance.sqrt();
        (d <= max_m).then_some((nn.item as u32, d))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snaps_to_nearest_within_radius() {
        let nodes = vec![[0.0, 0.0], [0.001, 0.0]]; // ~111m apart on the equator
        let s = Snapper::new(&nodes, 0.0);
        let (idx, d) = s.nearest([0.0001, 0.0], 200.0).unwrap();
        assert_eq!(idx, 0);
        assert!((d - 11.13).abs() < 0.5);
        assert!(s.nearest([0.01, 0.0], 200.0).is_none()); // ~1km away
    }
}
