//! The scale benchmark: many random systems, one random fault each.
//!
//! Complements the public benchmarks, which cover three small systems and
//! resource faults, with systems of arbitrary size and fault kinds they do
//! not include (crashes, error bursts, memory leaks, packet loss), and with
//! the *online* questions a benchmark of offline windows cannot answer: is
//! the fault detected, how fast, and how many false alarms precede it.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::Serialize;

use crate::scenario::{Outcome, Scenario};

/// Aggregated results for one group of scenarios.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Summary {
    /// Scenarios run.
    pub scenarios: usize,
    /// Faults that opened an incident.
    pub detected: usize,
    /// Median detection delay of the detected faults, seconds.
    pub median_delay_s: Option<f64>,
    /// Faulty service ranked first by the first analysis of the incident
    /// (what the on-call engineer sees within seconds).
    pub top1: usize,
    /// Faulty service in the top three of the first analysis.
    pub top3: usize,
    /// Faulty service ranked first by the final analysis.
    pub final_top1: usize,
    /// Incidents opened before the fault, over all scenarios.
    pub false_incidents: usize,
    /// Spans processed.
    pub spans: u64,
}

impl Summary {
    fn add(&mut self, o: &Outcome, delays: &mut Vec<f64>) {
        self.scenarios += 1;
        self.spans += o.spans;
        self.false_incidents += o.false_incidents;
        if let Some(d) = o.detection_delay_s {
            self.detected += 1;
            delays.push(d);
        }
        let rank = o.first_rank;
        self.top1 += usize::from(rank == Some(1));
        self.top3 += usize::from(rank.is_some_and(|r| r <= 3));
        self.final_top1 += usize::from(o.last_rank.or(o.first_rank) == Some(1));
    }
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    Some(v[v.len() / 2])
}

/// Runs `per_size` random scenarios for every size, cycling through the
/// fault kinds so that each kind is equally represented, on `workers`
/// threads. Results are returned in a deterministic order.
#[must_use]
pub fn run(sizes: &[usize], per_size: usize, seed: u64, workers: usize) -> Vec<Outcome> {
    let jobs: Vec<(usize, u64, usize)> = sizes
        .iter()
        .flat_map(|&n| (0..per_size).map(move |k| (n, seed.wrapping_mul(1_000_003).wrapping_add(k as u64), k)))
        .collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results = std::sync::Mutex::new(vec![None; jobs.len()]);
    std::thread::scope(|scope| {
        for _ in 0..workers.max(1) {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(&(n, s, kind)) = jobs.get(i) else { break };
                    let scenario = Scenario::random_with_kind(n, s, kind, 900.0, 300.0);
                    let outcome = scenario.run(Scenario::engine_config()).ok();
                    results.lock().unwrap_or_else(std::sync::PoisonError::into_inner)[i] = outcome;
                }
            });
        }
    });
    results.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner).into_iter().flatten().collect()
}

/// Groups outcomes by system size and by fault kind.
#[must_use]
pub fn summarise(outcomes: &[Outcome]) -> (BTreeMap<usize, Summary>, BTreeMap<String, Summary>) {
    let mut by_size: BTreeMap<usize, (Summary, Vec<f64>)> = BTreeMap::new();
    let mut by_kind: BTreeMap<String, (Summary, Vec<f64>)> = BTreeMap::new();
    for o in outcomes {
        let (s, d) = by_size.entry(o.services).or_default();
        s.add(o, d);
        let kind = o.fault.as_ref().map_or("none", |f| f.kind.name()).to_owned();
        let (s, d) = by_kind.entry(kind).or_default();
        s.add(o, d);
    }
    let finish = |(mut s, d): (Summary, Vec<f64>)| {
        s.median_delay_s = median(d);
        s
    };
    (
        by_size.into_iter().map(|(k, v)| (k, finish(v))).collect(),
        by_kind.into_iter().map(|(k, v)| (k, finish(v))).collect(),
    )
}

/// Renders the summaries as Markdown tables.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn markdown(outcomes: &[Outcome]) -> String {
    let (by_size, by_kind) = summarise(outcomes);
    let mut out = String::new();
    let row = |out: &mut String, key: &str, s: &Summary| {
        let pct = |x: usize| 100.0 * x as f64 / s.scenarios.max(1) as f64;
        let _ = writeln!(
            out,
            "| {key} | {} | {:.0}% | {} | {:.0}% | {:.0}% | {:.0}% | {} |",
            s.scenarios,
            pct(s.detected),
            s.median_delay_s.map_or("-".into(), |d| format!("{d:.0} s")),
            pct(s.top1),
            pct(s.top3),
            pct(s.final_top1),
            s.false_incidents,
        );
    };
    let header = "| detected | median delay | AC@1 | AC@3 | AC@1 final | false incidents |";
    let _ = writeln!(out, "| services | scenarios {header}\n|---:|---:|---:|---:|---:|---:|---:|---:|");
    for (n, s) in &by_size {
        row(&mut out, &n.to_string(), s);
    }
    let _ = writeln!(out, "\n| fault | scenarios {header}\n|---|---:|---:|---:|---:|---:|---:|---:|");
    for (k, s) in &by_kind {
        row(&mut out, k, s);
    }
    out
}
