use crate::osm::Segment;
use crate::weights;

/// Union-find with path halving.
struct Dsu(Vec<u32>);

impl Dsu {
    fn new(n: usize) -> Self {
        Dsu((0..n as u32).collect())
    }
    fn find(&mut self, mut x: u32) -> u32 {
        while self.0[x as usize] != x {
            self.0[x as usize] = self.0[self.0[x as usize] as usize];
            x = self.0[x as usize];
        }
        x
    }
    fn union(&mut self, a: u32, b: u32) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.0[ra as usize] = rb;
        }
    }
}

/// Drop weakly-connected components smaller than `min_comp` nodes and remap ids.
pub fn prune(node_ll: Vec<[f64; 2]>, segments: Vec<Segment>, min_comp: usize) -> (Vec<[f64; 2]>, Vec<Segment>) {
    let n = node_ll.len();
    let mut dsu = Dsu::new(n);
    for s in &segments {
        dsu.union(s.a, s.b);
    }
    let mut comp_size = vec![0u32; n];
    for i in 0..n as u32 {
        comp_size[dsu.find(i) as usize] += 1;
    }
    let mut remap = vec![u32::MAX; n];
    let mut kept_ll = Vec::new();
    for i in 0..n {
        if comp_size[dsu.find(i as u32) as usize] as usize >= min_comp {
            remap[i] = kept_ll.len() as u32;
            kept_ll.push(node_ll[i]);
        }
    }
    let kept_segs: Vec<Segment> = segments
        .into_iter()
        .filter_map(|s| {
            let (a, b) = (remap[s.a as usize], remap[s.b as usize]);
            (a != u32::MAX && b != u32::MAX).then_some(Segment { a, b, ..s })
        })
        .collect();
    eprintln!("  prune: kept {}/{} nodes, {} segments", kept_ll.len(), n, kept_segs.len());
    (kept_ll, kept_segs)
}

/// CSR of the REVERSED weighted graph: an arc v→u with weight w for every
/// forward walking arc u→v. Multi-source Dijkstra from feature sites over this
/// graph yields, for every node, the time to walk FROM that node TO its
/// nearest site — as the spec requires.
pub struct RevCsr {
    pub offsets: Vec<u32>,
    /// (target node, weight in deciseconds)
    pub arcs: Vec<(u32, u32)>,
}

impl RevCsr {
    pub fn n(&self) -> usize {
        self.offsets.len() - 1
    }
    pub fn out(&self, v: u32) -> &[(u32, u32)] {
        &self.arcs[self.offsets[v as usize] as usize..self.offsets[v as usize + 1] as usize]
    }
}

pub fn build_rev_csr(node_ll: &[[f64; 2]], elev: &[f32], segments: &[Segment]) -> RevCsr {
    let n = node_ll.len();
    // forward arcs: a→b with t_ab, b→a with t_ba.
    // reversed arcs: b→a with t_ab, a→b with t_ba.
    let mut deg = vec![0u32; n + 1];
    for s in segments {
        deg[s.a as usize + 1] += 1;
        deg[s.b as usize + 1] += 1;
    }
    for i in 0..n {
        deg[i + 1] += deg[i];
    }
    let offsets = deg;
    let mut fill = offsets.clone();
    let mut arcs = vec![(0u32, 0u32); offsets[n] as usize];
    for s in segments {
        let (pa, pb) = (node_ll[s.a as usize], node_ll[s.b as usize]);
        let d = weights::haversine_m(pa, pb);
        let dh = (elev[s.b as usize] - elev[s.a as usize]) as f64;
        let (t_ab, t_ba) = weights::segment_times_ds(d, dh, s.steps, s.flat);
        // reversed: arc from b to a carries the forward a→b time
        arcs[fill[s.b as usize] as usize] = (s.a, t_ab);
        fill[s.b as usize] += 1;
        arcs[fill[s.a as usize] as usize] = (s.b, t_ba);
        fill[s.a as usize] += 1;
    }
    RevCsr { offsets, arcs }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(a: u32, b: u32) -> Segment {
        Segment { a, b, steps: false, flat: false }
    }

    #[test]
    fn prune_small_components() {
        // component A: 0-1-2 (3 nodes), component B: 3-4 (2 nodes)
        let ll = vec![[0.0, 0.0]; 5];
        let segs = vec![seg(0, 1), seg(1, 2), seg(3, 4)];
        let (kept, ks) = prune(ll, segs, 3);
        assert_eq!(kept.len(), 3);
        assert_eq!(ks.len(), 2);
    }

    #[test]
    fn rev_csr_directions() {
        // two nodes 100m apart, b is 10m higher: walking a→b is slower than b→a,
        // so in the REVERSED graph the arc b→a must carry the slower (uphill) time.
        let ll = vec![[0.0, 0.0], [0.0, 0.0009]]; // ~100m apart
        let elev = vec![0.0, 10.0];
        let csr = build_rev_csr(&ll, &elev, &[seg(0, 1)]);
        let t_rev_b_to_a = csr.out(1)[0].1; // forward a→b (uphill)
        let t_rev_a_to_b = csr.out(0)[0].1; // forward b→a (downhill)
        assert!(t_rev_b_to_a > t_rev_a_to_b);
    }
}
