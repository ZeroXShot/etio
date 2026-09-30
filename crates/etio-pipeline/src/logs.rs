//! Log template mining and severity classification.
//!
//! Raw log lines are unbounded free text; root-cause analysis needs counts of
//! *kinds* of messages. [`Drain`] (He, Zhu, Zheng and Lyu, "Drain: An Online
//! Log Parsing Approach with Fixed Depth Tree", ICWS 2017) groups lines into
//! templates online: lines are routed through a fixed-depth tree keyed by
//! token count and leading tokens, then matched against the templates in the
//! leaf by token-wise similarity. Tokens that vary are replaced by `<*>`.
//!
//! Variable detection is done with a hand-written classifier instead of
//! regular expressions: it is several times faster, allocation-free, and
//! deterministic across platforms. Memory is bounded: past `max_clusters`
//! the least recently used template is evicted.

use hashbrown::HashMap;
use serde::{Deserialize, Serialize};

/// Placeholder for a variable token.
pub const WILDCARD: &str = "<*>";

/// Drain parameters.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DrainConfig {
    /// Tree depth, counting the root and the leaf (so `depth - 2` token layers).
    pub depth: usize,
    /// Minimum similarity for a line to join an existing template.
    pub similarity: f64,
    /// Maximum children per inner node; extra tokens share a wildcard child.
    pub max_children: usize,
    /// Maximum number of templates kept.
    pub max_clusters: usize,
    /// Lines longer than this many tokens are truncated.
    pub max_tokens: usize,
}

impl Default for DrainConfig {
    fn default() -> Self {
        Self { depth: 4, similarity: 0.4, max_children: 100, max_clusters: 5_000, max_tokens: 64 }
    }
}

/// A log template.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cluster {
    /// Stable identifier (never reused after eviction).
    pub id: u32,
    /// Template tokens; variable positions are [`WILDCARD`].
    pub template: Vec<Box<str>>,
    /// Lines matched so far.
    pub count: u64,
    /// Timestamp of the first line, ns since the epoch.
    pub first_seen: i64,
    last_used: u64,
}

impl Cluster {
    /// The template as a single string.
    #[must_use]
    pub fn text(&self) -> String {
        self.template.join(" ")
    }
}

/// Result of adding a line.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Match {
    /// Template the line was assigned to.
    pub cluster: u32,
    /// The line created a new template.
    pub created: bool,
    /// The line generalised an existing template.
    pub changed: bool,
}

#[derive(Debug, Default, Clone)]
struct Node {
    children: HashMap<Box<str>, Node>,
    clusters: Vec<u32>,
}

/// Online log template miner.
#[derive(Debug, Clone)]
pub struct Drain {
    cfg: DrainConfig,
    root: HashMap<usize, Node>,
    clusters: HashMap<u32, Cluster>,
    next_id: u32,
    tick: u64,
    evicted: u64,
}

/// Whether a token is a variable (number, identifier, address, ...).
#[must_use]
pub fn is_variable(token: &str) -> bool {
    let bytes = token.as_bytes();
    if bytes.iter().any(u8::is_ascii_digit) {
        return true;
    }
    // Long hexadecimal-looking words without digits are rare but exist
    // (e.g. "deadbeefcafe"); require a length that makes a word unlikely.
    bytes.len() >= 12 && bytes.iter().all(u8::is_ascii_hexdigit)
}

fn tokenize(line: &str, max: usize) -> Vec<&str> {
    line.split_ascii_whitespace().take(max).collect()
}

impl Drain {
    /// Creates an empty miner.
    #[must_use]
    pub fn new(cfg: DrainConfig) -> Self {
        Self {
            cfg: DrainConfig { depth: cfg.depth.max(3), ..cfg },
            root: HashMap::new(),
            clusters: HashMap::new(),
            next_id: 0,
            tick: 0,
            evicted: 0,
        }
    }

    /// Number of live templates.
    #[must_use]
    pub fn len(&self) -> usize {
        self.clusters.len()
    }

    /// Whether no template exists.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.clusters.is_empty()
    }

    /// Templates evicted to respect the memory bound.
    #[must_use]
    pub const fn evicted(&self) -> u64 {
        self.evicted
    }

    /// Looks up a template.
    #[must_use]
    pub fn cluster(&self, id: u32) -> Option<&Cluster> {
        self.clusters.get(&id)
    }

    /// All live templates, in id order.
    #[must_use]
    pub fn clusters(&self) -> Vec<&Cluster> {
        let mut v: Vec<&Cluster> = self.clusters.values().collect();
        v.sort_by_key(|c| c.id);
        v
    }

    /// Adds a line observed at `ts` (ns since the epoch).
    pub fn add(&mut self, line: &str, ts: i64) -> Match {
        self.tick += 1;
        let tokens = tokenize(line, self.cfg.max_tokens);
        let masked: Vec<&str> = tokens.iter().map(|t| if is_variable(t) { WILDCARD } else { *t }).collect();

        let depth = self.cfg.depth;
        let max_children = self.cfg.max_children;
        let leaf = {
            let mut node = self.root.entry(masked.len()).or_default();
            for &tok in masked.iter().take(depth - 2) {
                let key: &str = if node.children.contains_key(tok) {
                    tok
                } else if node.children.len() < max_children {
                    node.children.insert(tok.into(), Node::default());
                    tok
                } else {
                    node.children.entry(WILDCARD.into()).or_default();
                    WILDCARD
                };
                node = node.children.get_mut(key).unwrap_or_else(|| unreachable!("inserted above"));
            }
            node
        };

        // Best match in the leaf.
        let mut best: Option<(u32, f64, usize)> = None;
        for &id in &leaf.clusters {
            let Some(c) = self.clusters.get(&id) else { continue };
            let (sim, params) = similarity(&c.template, &masked);
            let better = match best {
                None => true,
                Some((_, bs, bp)) => sim.total_cmp(&bs).then(params.cmp(&bp)).is_gt(),
            };
            if better {
                best = Some((id, sim, params));
            }
        }

        if let Some((id, sim, _)) = best
            && sim >= self.cfg.similarity
        {
            let tick = self.tick;
            let c = self.clusters.get_mut(&id).unwrap_or_else(|| unreachable!("id from leaf"));
            let mut changed = false;
            for (slot, tok) in c.template.iter_mut().zip(&masked) {
                if &**slot != *tok && &**slot != WILDCARD {
                    *slot = WILDCARD.into();
                    changed = true;
                }
            }
            c.count += 1;
            c.last_used = tick;
            return Match { cluster: id, created: false, changed };
        }

        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        leaf.clusters.push(id);
        self.clusters.insert(
            id,
            Cluster {
                id,
                template: masked.iter().map(|t| (*t).into()).collect(),
                count: 1,
                first_seen: ts,
                last_used: self.tick,
            },
        );
        if self.clusters.len() > self.cfg.max_clusters {
            self.evict_lru();
        }
        Match { cluster: id, created: true, changed: false }
    }

    fn evict_lru(&mut self) {
        let Some(victim) = self.clusters.values().min_by_key(|c| c.last_used).map(|c| c.id) else {
            return;
        };
        self.clusters.remove(&victim);
        fn purge(node: &mut Node, id: u32) -> bool {
            if let Some(pos) = node.clusters.iter().position(|&c| c == id) {
                node.clusters.swap_remove(pos);
                return true;
            }
            node.children.values_mut().any(|child| purge(child, id))
        }
        for node in self.root.values_mut() {
            if purge(node, victim) {
                break;
            }
        }
        self.evicted += 1;
    }
}

/// Fraction of positions where the template and the line agree, and the
/// number of generalised positions (used to break ties).
///
/// A masked variable in the line (already `<*>`) agrees with a `<*>` in the
/// template, as a masked `<NUM>` agrees with `<NUM>` in Drain3. A template
/// position generalised to `<*>` does not agree with a constant token: it
/// counts as a parameter, as in Drain's `seqDist`.
fn similarity(template: &[Box<str>], tokens: &[&str]) -> (f64, usize) {
    if template.len() != tokens.len() || tokens.is_empty() {
        return (if template.is_empty() && tokens.is_empty() { 1.0 } else { 0.0 }, 0);
    }
    let mut same = 0usize;
    let mut params = 0usize;
    for (t, tok) in template.iter().zip(tokens) {
        if &**t == *tok {
            same += 1;
        } else if &**t == WILDCARD {
            params += 1;
        }
    }
    #[allow(clippy::cast_precision_loss)]
    (same as f64 / tokens.len() as f64, params)
}

/// Coarse severity of a log line.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Anything that is not a warning or an error.
    Info,
    /// A warning.
    Warn,
    /// An error.
    Error,
}

impl Severity {
    /// Maps an OTLP severity number (1-24).
    #[must_use]
    pub const fn from_otlp(number: i32) -> Option<Self> {
        match number {
            1..=12 => Some(Self::Info),
            13..=16 => Some(Self::Warn),
            17..=24 => Some(Self::Error),
            _ => None,
        }
    }

    /// Classifies free text by keywords, for logs without a severity field.
    #[must_use]
    pub fn classify(line: &str) -> Self {
        let mut warn = false;
        for word in line.split(|c: char| !c.is_ascii_alphanumeric()) {
            if word.len() < 4 || word.len() > 64 {
                continue;
            }
            let w = word.to_ascii_lowercase();
            // Exception and error class names: NullPointerException, ValueError.
            if w.len() > 9 && (w.ends_with("exception") || w.ends_with("error")) {
                return Self::Error;
            }
            match w.as_str() {
                "error" | "errors" | "exception" | "fail" | "failed" | "failure" | "fatal" | "panic" | "refused"
                | "timeout" | "unavailable" | "traceback" | "crash" | "crashed" | "severe" | "critical" => {
                    return Self::Error;
                }
                "warn" | "warning" => warn = true,
                _ => {}
            }
        }
        if warn { Self::Warn } else { Self::Info }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_lines_into_templates() {
        let mut d = Drain::new(DrainConfig::default());
        let a = d.add("user 42 logged in from 10.0.0.1", 1);
        let b = d.add("user 7 logged in from 10.0.0.9", 2);
        let c = d.add("payment declined for order 99", 3);
        assert!(a.created);
        assert_eq!(a.cluster, b.cluster);
        assert!(!b.created);
        assert_ne!(a.cluster, c.cluster);
        assert_eq!(d.cluster(a.cluster).unwrap().text(), "user <*> logged in from <*>");
        assert_eq!(d.cluster(a.cluster).unwrap().count, 2);
        assert_eq!(d.cluster(a.cluster).unwrap().first_seen, 1);
    }

    #[test]
    fn generalises_differing_constant_tokens() {
        let mut d = Drain::new(DrainConfig::default());
        let a = d.add("connection to cart established quickly", 1);
        let b = d.add("connection to redis established quickly", 2);
        assert_eq!(a.cluster, b.cluster);
        assert!(b.changed);
        assert_eq!(d.cluster(a.cluster).unwrap().text(), "connection to <*> established quickly");
    }

    #[test]
    fn masked_variables_match_template_wildcards() {
        // Stack frames: every token but "at" is a variable.
        let mut d = Drain::new(DrainConfig::default());
        let a = d.add("\tat sun.net.HttpClient.New(HttpClient.java:357) ~[na:1.8.0_372]", 0);
        let b = d.add("\tat zipkin.Reporter.run(Reporter.java:12) [zipkin-0.6.9.jar]", 1);
        assert_eq!(a.cluster, b.cluster);
        assert_eq!(d.len(), 1);
    }

    #[test]
    fn different_lengths_never_merge() {
        let mut d = Drain::new(DrainConfig::default());
        let a = d.add("request started", 1);
        let b = d.add("request started now", 2);
        assert_ne!(a.cluster, b.cluster);
        assert_eq!(d.len(), 2);
    }

    #[test]
    fn memory_is_bounded() {
        let mut d = Drain::new(DrainConfig { max_clusters: 10, ..DrainConfig::default() });
        let words = ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel"];
        for (i, a) in words.iter().enumerate() {
            for (j, b) in words.iter().enumerate() {
                d.add(&format!("{a} {b} event happened {i}{j}"), 0);
            }
        }
        assert!(d.len() <= 10);
        assert!(d.evicted() > 0);
        // Surviving templates are still consistent.
        for c in d.clusters() {
            assert_eq!(c.template.len(), 5);
        }
    }

    #[test]
    fn wildcard_child_absorbs_fan_out() {
        let mut d = Drain::new(DrainConfig { max_children: 2, ..DrainConfig::default() });
        for w in ["aa", "bb", "cc", "dd"] {
            d.add(&format!("{w} message body"), 0);
        }
        assert!(d.len() >= 2);
    }

    #[test]
    fn variable_detection() {
        assert!(is_variable("42"));
        assert!(is_variable("10.0.0.1"));
        assert!(is_variable("3fa85f64-5717-4562-b3fc-2c963f66afa6"));
        assert!(is_variable("deadbeefcafe"));
        assert!(!is_variable("checkout"));
        assert!(!is_variable("facade"));
    }

    #[test]
    fn severity_keywords() {
        assert_eq!(Severity::classify("GET /cart failed: connection refused"), Severity::Error);
        assert_eq!(Severity::classify("java.lang.NullPointerException at Foo"), Severity::Error);
        assert_eq!(Severity::classify("raise ValueError(x)"), Severity::Error);
        assert_eq!(Severity::classify("mirror of terror"), Severity::Info);
        assert_eq!(Severity::classify("Exception in thread main"), Severity::Error);
        assert_eq!(Severity::classify("WARNING: slow query"), Severity::Warn);
        assert_eq!(Severity::classify("request served in 3ms"), Severity::Info);
        assert_eq!(Severity::from_otlp(17), Some(Severity::Error));
        assert_eq!(Severity::from_otlp(0), None);
    }
}
