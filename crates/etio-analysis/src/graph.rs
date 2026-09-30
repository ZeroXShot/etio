//! The service dependency graph and random-walk scoring on it.
//!
//! Edges point from caller to callee. In a request-driven system faults
//! propagate *against* the edges: a slow callee makes its callers slow. A
//! root cause therefore tends to be an anomalous node whose callers are
//! anomalous and whose callees are not.

use std::collections::{HashMap, VecDeque};

use serde::{Deserialize, Serialize};

/// A directed caller → callee graph over named services.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(from = "GraphRepr", into = "GraphRepr")]
pub struct ServiceGraph {
    names: Vec<String>,
    index: HashMap<String, usize>,
    callees: Vec<Vec<usize>>,
    callers: Vec<Vec<usize>>,
    weight: HashMap<(usize, usize), f64>,
}

/// Serialised form: a list of weighted edges plus isolated nodes.
#[derive(Serialize, Deserialize)]
struct GraphRepr {
    nodes: Vec<String>,
    edges: Vec<(String, String, f64)>,
}

impl From<ServiceGraph> for GraphRepr {
    fn from(g: ServiceGraph) -> Self {
        let mut edges: Vec<(String, String, f64)> =
            g.edges().map(|(a, b, w)| (g.names[a].clone(), g.names[b].clone(), w)).collect();
        edges.sort_by(|x, y| (&x.0, &x.1).cmp(&(&y.0, &y.1)));
        Self { nodes: g.names, edges }
    }
}

impl From<GraphRepr> for ServiceGraph {
    fn from(r: GraphRepr) -> Self {
        let mut g = Self::new();
        for n in &r.nodes {
            g.add_node(n);
        }
        for (a, b, w) in &r.edges {
            g.add_edge(a, b, *w);
        }
        g
    }
}

impl ServiceGraph {
    /// An empty graph.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a graph from `(caller, callee)` pairs with unit weight.
    #[must_use]
    pub fn from_edges<'a>(edges: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let mut g = Self::new();
        for (a, b) in edges {
            g.add_edge(a, b, 1.0);
        }
        g
    }

    /// Adds a node if absent and returns its index.
    pub fn add_node(&mut self, name: &str) -> usize {
        if let Some(&i) = self.index.get(name) {
            return i;
        }
        let i = self.names.len();
        self.names.push(name.to_owned());
        self.index.insert(name.to_owned(), i);
        self.callees.push(Vec::new());
        self.callers.push(Vec::new());
        i
    }

    /// Adds (or reinforces) a caller → callee edge. Self-loops are ignored.
    pub fn add_edge(&mut self, caller: &str, callee: &str, weight: f64) {
        if caller == callee {
            self.add_node(caller);
            return;
        }
        let a = self.add_node(caller);
        let b = self.add_node(callee);
        let w = self.weight.entry((a, b)).or_insert(0.0);
        if *w == 0.0 {
            insert_sorted(&mut self.callees[a], b);
            insert_sorted(&mut self.callers[b], a);
        }
        *w += if weight.is_finite() && weight > 0.0 { weight } else { 1.0 };
    }

    /// Index of a node by name.
    #[must_use]
    pub fn node(&self, name: &str) -> Option<usize> {
        self.index.get(name).copied()
    }

    /// Name of a node.
    #[must_use]
    pub fn name(&self, i: usize) -> &str {
        &self.names[i]
    }

    /// All node names, in index order.
    #[must_use]
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// Number of nodes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// Whether the graph has no nodes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// Direct callees of a node.
    #[must_use]
    pub fn callees(&self, i: usize) -> &[usize] {
        &self.callees[i]
    }

    /// Direct callers of a node.
    #[must_use]
    pub fn callers(&self, i: usize) -> &[usize] {
        &self.callers[i]
    }

    /// Weight of an edge (0 if absent).
    #[must_use]
    pub fn weight(&self, caller: usize, callee: usize) -> f64 {
        self.weight.get(&(caller, callee)).copied().unwrap_or(0.0)
    }

    /// All edges as `(caller, callee, weight)`, in index order.
    pub fn edges(&self) -> impl Iterator<Item = (usize, usize, f64)> + '_ {
        self.callees.iter().enumerate().flat_map(move |(a, cs)| cs.iter().map(move |&b| (a, b, self.weight(a, b))))
    }

    /// Nodes that nobody calls: the entry points of the system.
    #[must_use]
    pub fn entry_points(&self) -> Vec<usize> {
        (0..self.len()).filter(|&i| self.callers[i].is_empty()).collect()
    }

    /// Transitive callers of `i` (excluding `i`), in BFS order.
    #[must_use]
    pub fn ancestors(&self, i: usize) -> Vec<usize> {
        self.bfs(i, &self.callers)
    }

    /// Transitive callees of `i` (excluding `i`), in BFS order.
    #[must_use]
    pub fn descendants(&self, i: usize) -> Vec<usize> {
        self.bfs(i, &self.callees)
    }

    fn bfs(&self, start: usize, adj: &[Vec<usize>]) -> Vec<usize> {
        let mut seen = vec![false; self.len()];
        seen[start] = true;
        let mut out = Vec::new();
        let mut queue = VecDeque::from([start]);
        while let Some(u) = queue.pop_front() {
            for &v in &adj[u] {
                if !seen[v] {
                    seen[v] = true;
                    out.push(v);
                    queue.push_back(v);
                }
            }
        }
        out
    }
}

fn insert_sorted(v: &mut Vec<usize>, x: usize) {
    if let Err(pos) = v.binary_search(&x) {
        v.insert(pos, x);
    }
}

/// Parameters of [`anomaly_random_walk`].
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WalkConfig {
    /// Relative weight of moving from a callee back to a caller.
    pub backward: f64,
    /// Probability of following an edge rather than restarting.
    pub damping: f64,
    /// Maximum power iterations.
    pub iterations: usize,
    /// L1 convergence tolerance.
    pub tolerance: f64,
}

impl Default for WalkConfig {
    fn default() -> Self {
        Self { backward: 0.2, damping: 0.85, iterations: 200, tolerance: 1e-10 }
    }
}

/// Random walk with restart on the anomaly-weighted graph.
///
/// `scores[i] ≥ 0` is the anomaly strength of node `i`. A walker at `u`
/// moves to a callee `v` with weight `S(v)` (towards potential causes), back
/// to a caller `w` with weight `ρ S(w)`, and stays with weight
/// `max(0, S(u) − max S(neighbours))`: a node more anomalous than everything
/// around it retains the walker. Restarts follow the anomaly scores. The
/// stationary distribution concentrates on nodes that explain the anomalies
/// around them. This is the second-order walk of MicroCause/CloudRanger
/// without the partial-correlation edge weights, which need far more data
/// than an incident provides.
///
/// Returns a probability vector (sums to one), or uniform weights if every
/// score is zero.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn anomaly_random_walk(graph: &ServiceGraph, scores: &[f64], cfg: &WalkConfig) -> Vec<f64> {
    let n = graph.len();
    assert_eq!(scores.len(), n, "one score per node");
    if n == 0 {
        return Vec::new();
    }
    let s: Vec<f64> = scores.iter().map(|x| if x.is_finite() { x.max(0.0) } else { 0.0 }).collect();
    let total: f64 = s.iter().sum();
    if total <= 0.0 {
        return vec![1.0 / n as f64; n];
    }
    let restart: Vec<f64> = s.iter().map(|x| x / total).collect();

    // Row-stochastic transitions as sparse rows of (target, probability).
    let rows: Vec<Vec<(usize, f64)>> = (0..n)
        .map(|u| {
            let mut row: Vec<(usize, f64)> = Vec::new();
            let mut neighbour_max: f64 = 0.0;
            for &v in graph.callees(u) {
                row.push((v, s[v]));
                neighbour_max = neighbour_max.max(s[v]);
            }
            for &w in graph.callers(u) {
                row.push((w, cfg.backward * s[w]));
                neighbour_max = neighbour_max.max(s[w]);
            }
            row.push((u, (s[u] - neighbour_max).max(0.0)));
            let sum: f64 = row.iter().map(|(_, w)| w).sum();
            if sum > 0.0 {
                row.iter_mut().for_each(|(_, w)| *w /= sum);
                row.retain(|(_, w)| *w > 0.0);
                row
            } else {
                Vec::new() // dangling: all mass restarts
            }
        })
        .collect();

    let mut pi = restart.clone();
    let mut next = vec![0.0; n];
    for _ in 0..cfg.iterations {
        next.iter_mut().for_each(|x| *x = 0.0);
        let mut dangling = 0.0;
        for (u, row) in rows.iter().enumerate() {
            if row.is_empty() {
                dangling += pi[u];
                continue;
            }
            for &(v, p) in row {
                next[v] += cfg.damping * pi[u] * p;
            }
        }
        // Teleportation plus the mass of dangling nodes both follow the restart vector.
        let restart_mass = (1.0 - cfg.damping) + cfg.damping * dangling;
        for (x, r) in next.iter_mut().zip(&restart) {
            *x += restart_mass * r;
        }
        let delta: f64 = next.iter().zip(&pi).map(|(a, b)| (a - b).abs()).sum();
        std::mem::swap(&mut pi, &mut next);
        if delta < cfg.tolerance {
            break;
        }
    }
    let sum: f64 = pi.iter().sum();
    pi.iter_mut().for_each(|x| *x /= sum);
    pi
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain() -> ServiceGraph {
        // frontend -> checkout -> payment, frontend -> catalog
        ServiceGraph::from_edges([("frontend", "checkout"), ("checkout", "payment"), ("frontend", "catalog")])
    }

    #[test]
    fn structure_queries() {
        let g = chain();
        let f = g.node("frontend").unwrap();
        let p = g.node("payment").unwrap();
        assert_eq!(g.entry_points(), vec![f]);
        assert_eq!(g.descendants(f).len(), 3);
        assert_eq!(g.ancestors(p).len(), 2);
        assert!(g.callees(p).is_empty());
        assert!(g.node("nope").is_none());
    }

    #[test]
    fn duplicate_edges_accumulate_weight_and_self_loops_are_ignored() {
        let mut g = ServiceGraph::new();
        g.add_edge("a", "b", 2.0);
        g.add_edge("a", "b", 3.0);
        g.add_edge("a", "a", 1.0);
        let (a, b) = (g.node("a").unwrap(), g.node("b").unwrap());
        assert!((g.weight(a, b) - 5.0).abs() < 1e-12);
        assert_eq!(g.callees(a), &[b]);
        assert!(g.callees(b).is_empty());
        assert_eq!(g.edges().count(), 1);
    }

    #[test]
    fn serde_round_trip() {
        let g = chain();
        let json = serde_json::to_string(&g).unwrap();
        let back: ServiceGraph = serde_json::from_str(&json).unwrap();
        assert_eq!(g, back);
    }

    #[test]
    fn walk_concentrates_on_the_deepest_anomalous_node() {
        let g = chain();
        let idx = |n: &str| g.node(n).unwrap();
        let mut scores = vec![0.0; g.len()];
        // Payment is the root; checkout and frontend inherit its latency.
        scores[idx("payment")] = 1.0;
        scores[idx("checkout")] = 0.8;
        scores[idx("frontend")] = 0.7;
        let pi = anomaly_random_walk(&g, &scores, &WalkConfig::default());
        let best = (0..g.len()).max_by(|&a, &b| pi[a].total_cmp(&pi[b])).unwrap();
        assert_eq!(g.name(best), "payment");
        assert!((pi.iter().sum::<f64>() - 1.0).abs() < 1e-9);
        assert!(pi[idx("catalog")] < 1e-9, "a healthy node receives no mass");
    }

    #[test]
    fn walk_with_no_anomalies_is_uniform() {
        let g = chain();
        let pi = anomaly_random_walk(&g, &[0.0; 4], &WalkConfig::default());
        assert!(pi.iter().all(|p| (p - 0.25).abs() < 1e-12));
    }
}
