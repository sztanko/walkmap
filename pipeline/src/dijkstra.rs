use crate::graph::RevCsr;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

pub const UNREACHED: u32 = u32::MAX;

/// Multi-source Dijkstra over the reversed graph.
/// `sites` = (graph node, site id). Returns per node: winning site id,
/// walking time (deciseconds) from that node to the site, and the NEXT HOP —
/// the first node on the node's forward walk toward its site
/// (UNREACHED for site nodes and unreached nodes).
pub fn partition(csr: &RevCsr, sites: &[(u32, u32)]) -> (Vec<u32>, Vec<u32>, Vec<u32>) {
    let n = csr.n();
    let mut label = vec![UNREACHED; n];
    let mut dist = vec![u32::MAX; n];
    let mut next_hop = vec![UNREACHED; n];
    let mut heap: BinaryHeap<Reverse<(u32, u32)>> = BinaryHeap::new();
    for &(node, site) in sites {
        if dist[node as usize] > 0 {
            dist[node as usize] = 0;
            label[node as usize] = site;
            heap.push(Reverse((0, node)));
        }
    }
    while let Some(Reverse((d, v))) = heap.pop() {
        if d > dist[v as usize] {
            continue;
        }
        let l = label[v as usize];
        for &(u, w) in csr.out(v) {
            let nd = d.saturating_add(w);
            if nd < dist[u as usize] {
                dist[u as usize] = nd;
                label[u as usize] = l;
                // reversed arc v→u = forward walking arc u→v
                next_hop[u as usize] = v;
                heap.push(Reverse((nd, u)));
            }
        }
    }
    (label, dist, next_hop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::build_rev_csr;
    use crate::osm::Segment;

    #[test]
    fn two_sites_split_a_line() {
        // 5 nodes in a line, ~100m apart, flat. Sites at nodes 0 and 4.
        let ll: Vec<[f64; 2]> = (0..5).map(|i| [0.0, 0.0009 * i as f64]).collect();
        let elev = vec![0.0; 5];
        let segs: Vec<Segment> =
            (0..4).map(|i| Segment { a: i, b: i + 1, steps: false, flat: false }).collect();
        let csr = build_rev_csr(&ll, &elev, &segs);
        let (label, dist, next_hop) = partition(&csr, &[(0, 0), (4, 1)]);
        assert_eq!(label[0], 0);
        assert_eq!(label[1], 0);
        assert_eq!(label[3], 1);
        assert_eq!(label[4], 1);
        assert_eq!(dist[0], 0);
        assert!(dist[1] > 0);
        // next hops walk toward the winning site
        assert_eq!(next_hop[1], 0);
        assert_eq!(next_hop[3], 4);
        assert_eq!(next_hop[0], UNREACHED, "site nodes are terminal");
        assert_eq!(next_hop[4], UNREACHED);
    }
}
