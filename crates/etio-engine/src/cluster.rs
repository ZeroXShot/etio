//! The edge/core protocol, as pure state machines.
//!
//! In a distributed deployment, *edge* nodes receive telemetry, assemble
//! traces and aggregate windows; they ship the sealed [`WindowSummary`]s to
//! one or more *core* nodes, which merge them and run detection and analysis.
//! Summaries are kilobytes where raw spans are gigabytes, and they merge
//! exactly (see [`crate::window`]).
//!
//! This module contains the protocol logic without any I/O, so that it can
//! be tested exhaustively by deterministic simulation:
//!
//! * [`Outbox`] (edge side) numbers summaries with a per-epoch sequence,
//!   keeps them until every core has acknowledged them, and replays the
//!   unacknowledged suffix after a reconnection. Memory is bounded: when a
//!   core stays unreachable too long, the oldest summaries are dropped and
//!   counted.
//! * [`Collector`] (core side) deduplicates by `(edge, epoch, sequence)`,
//!   merges partial summaries of the same window from different edges, and
//!   releases windows in order once every live edge has reported them (each
//!   edge's latest window acts as its watermark; edges send empty summaries
//!   as heartbeats). An edge that stops reporting is declared dead after a
//!   timeout, and windows then close on a deadline instead of waiting for it.
//!
//! Delivery is at-least-once and application is exactly-once: a summary is
//! applied at most once per core, and cores fed the same summaries reach the
//! same state, without consensus.

use std::collections::{BTreeMap, VecDeque};

use hashbrown::HashMap;
use serde::{Deserialize, Serialize};

use crate::window::WindowSummary;

/// A sequenced summary awaiting acknowledgement.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// Sequence number within the edge's epoch (starts at 1).
    pub seq: u64,
    /// The summary.
    pub summary: WindowSummary,
}

/// Counters of an [`Outbox`].
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxStats {
    /// Summaries enqueued.
    pub enqueued: u64,
    /// Summaries dropped because the outbox was full.
    pub dropped: u64,
}

/// The edge side: sequencing, retention and replay.
#[derive(Debug)]
pub struct Outbox {
    edge_id: String,
    epoch: u64,
    next_seq: u64,
    entries: VecDeque<Entry>,
    /// Highest sequence acknowledged by each core.
    acked: Vec<u64>,
    capacity: usize,
    stats: OutboxStats,
}

impl Outbox {
    /// Creates an outbox for `cores` destinations. `epoch` must differ from
    /// any epoch this edge used before (a start timestamp works).
    #[must_use]
    pub fn new(edge_id: impl Into<String>, epoch: u64, cores: usize, capacity: usize) -> Self {
        Self {
            edge_id: edge_id.into(),
            epoch,
            next_seq: 1,
            entries: VecDeque::new(),
            acked: vec![0; cores.max(1)],
            capacity: capacity.max(1),
            stats: OutboxStats::default(),
        }
    }

    /// Edge identity.
    #[must_use]
    pub fn edge_id(&self) -> &str {
        &self.edge_id
    }

    /// Epoch of this edge incarnation.
    #[must_use]
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Counters.
    #[must_use]
    pub const fn stats(&self) -> OutboxStats {
        self.stats
    }

    /// Entries retained.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is retained.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Enqueues a sealed summary.
    pub fn push(&mut self, summary: WindowSummary) {
        if self.entries.len() == self.capacity {
            self.entries.pop_front();
            self.stats.dropped += 1;
        }
        self.entries.push_back(Entry { seq: self.next_seq, summary });
        self.next_seq += 1;
        self.stats.enqueued += 1;
    }

    /// Records a cumulative acknowledgement from core `core`.
    pub fn ack(&mut self, core: usize, seq: u64) {
        if let Some(a) = self.acked.get_mut(core) {
            *a = (*a).max(seq.min(self.next_seq - 1));
        }
        let all = self.acked.iter().copied().min().unwrap_or(0);
        while self.entries.front().is_some_and(|e| e.seq <= all) {
            self.entries.pop_front();
        }
    }

    /// Entries core `core` has not acknowledged yet, oldest first. After a
    /// reconnection the transport replays from here.
    pub fn unacked(&self, core: usize) -> impl Iterator<Item = &Entry> {
        let from = self.acked.get(core).copied().unwrap_or(0);
        self.entries.iter().filter(move |e| e.seq > from)
    }

    /// The highest sequence core `core` acknowledged.
    #[must_use]
    pub fn acked(&self, core: usize) -> u64 {
        self.acked.get(core).copied().unwrap_or(0)
    }

    /// The first sequence this edge can still send to core `core`. It is
    /// `acked(core) + 1` unless the outbox overflowed and dropped entries the
    /// core never acknowledged; it goes into the hello of every connection.
    #[must_use]
    pub fn first_unacked(&self, core: usize) -> u64 {
        self.unacked(core).next().map_or(self.next_seq, |e| e.seq)
    }
}

#[derive(Clone, Debug)]
struct EdgeState {
    epoch: u64,
    last_seq: u64,
    high_window: Option<i64>,
    last_seen: i64,
}

/// Counters of a [`Collector`].
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectorStats {
    /// Summaries merged.
    pub accepted: u64,
    /// Duplicates ignored (retransmissions).
    pub duplicates: u64,
    /// Out-of-order summaries refused (the edge will resend them in order).
    pub gaps: u64,
    /// Summaries from a superseded epoch.
    pub stale_epoch: u64,
    /// Summaries for windows already released (arrived after the deadline).
    pub late: u64,
    /// Windows released because the deadline passed, not because every edge reported.
    pub deadline_releases: u64,
    /// Summaries an edge declared lost (its outbox overflowed).
    pub lost: u64,
}

/// The result of offering a summary to a [`Collector`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Accept {
    /// Merged (or already merged); acknowledge `ack` to the edge.
    Ack(u64),
    /// Out of order; acknowledge `ack` (the last in-order sequence) so the
    /// edge replays from there.
    Gap(u64),
    /// From a superseded incarnation of the edge; ignore.
    Stale,
}

/// The core side: deduplication, merging and ordered release.
#[derive(Debug)]
pub struct Collector {
    resolution: i64,
    deadline: i64,
    liveness: i64,
    grace: i64,
    edges: HashMap<String, EdgeState>,
    pending: BTreeMap<i64, WindowSummary>,
    next_window: Option<i64>,
    /// When the first message arrived. Nothing is released during the
    /// following `grace`, so that every edge has a chance to announce
    /// itself before windows start closing.
    started_at: Option<i64>,
    stats: CollectorStats,
}

impl Collector {
    /// Creates a collector (times in ns).
    ///
    /// * `deadline` is measured in **event time**: a window that some edge
    ///   has not reported is released anyway once another edge has reported
    ///   a window `deadline` later. It bounds how far edges may drift apart
    ///   and how long a dead edge holds the others back, and it behaves the
    ///   same live and in a replay.
    /// * `liveness` is wall-clock: an edge silent for longer stops holding
    ///   windows back at all.
    /// * The start-up grace period (wall-clock, `deadline` by default, see
    ///   [`Collector::with_grace`]) gives every edge a chance to connect
    ///   before windows start closing.
    #[must_use]
    pub fn new(resolution: i64, deadline: i64, liveness: i64) -> Self {
        Self {
            resolution: resolution.max(1),
            deadline,
            liveness,
            grace: deadline,
            edges: HashMap::new(),
            pending: BTreeMap::new(),
            next_window: None,
            started_at: None,
            stats: CollectorStats::default(),
        }
    }

    /// Sets the start-up grace period (wall-clock ns).
    #[must_use]
    pub const fn with_grace(mut self, grace: i64) -> Self {
        self.grace = grace;
        self
    }

    /// Counters.
    #[must_use]
    pub const fn stats(&self) -> CollectorStats {
        self.stats
    }

    /// Edges known to the collector.
    pub fn edges(&self) -> impl Iterator<Item = (&str, u64, Option<i64>)> {
        self.edges.iter().map(|(k, v)| (k.as_str(), v.epoch, v.high_window))
    }

    /// Starts releasing at window `w` (after a restore, to skip windows the
    /// engine has already applied).
    pub fn start_at(&mut self, w: i64) {
        self.next_window = Some(self.next_window.map_or(w, |n| n.max(w)));
    }

    /// Registers (or re-registers) an edge and returns the sequence it should
    /// resume after. `first_seq` is the first sequence the edge can still
    /// send ([`Outbox::first_unacked`]):
    ///
    /// * for an edge this collector does not know (a new edge, or any edge
    ///   after the core restarted) it sets the starting point, since the
    ///   summaries before it were acknowledged by a previous incarnation of
    ///   the core or never existed;
    /// * for a known edge, a `first_seq` beyond the next expected sequence
    ///   means the edge dropped summaries; they are counted as lost and the
    ///   stream continues instead of waiting forever for them.
    ///
    /// Returns 0 for a new epoch.
    pub fn hello(&mut self, edge: &str, epoch: u64, first_seq: u64, now: i64) -> u64 {
        let known = self.edges.contains_key(edge);
        let resume = self.touch(edge, epoch, now);
        let Some(st) = self.edges.get_mut(edge) else { return resume };
        if epoch != st.epoch || first_seq <= resume + 1 {
            return resume;
        }
        if known {
            self.stats.lost += first_seq - resume - 1;
        }
        st.last_seq = first_seq - 1;
        st.last_seq
    }

    fn touch(&mut self, edge: &str, epoch: u64, now: i64) -> u64 {
        self.started_at.get_or_insert(now);
        let st = self.edges.entry(edge.to_owned()).or_insert(EdgeState {
            epoch,
            last_seq: 0,
            high_window: None,
            last_seen: now,
        });
        if epoch > st.epoch {
            *st = EdgeState { epoch, last_seq: 0, high_window: st.high_window, last_seen: now };
        }
        st.last_seen = now;
        if epoch == st.epoch { st.last_seq } else { 0 }
    }

    /// Offers a sequenced summary from an edge.
    pub fn accept(&mut self, edge: &str, epoch: u64, seq: u64, summary: WindowSummary, now: i64) -> Accept {
        let resume = self.touch(edge, epoch, now);
        let st = self.edges.get_mut(edge).unwrap_or_else(|| unreachable!("registered by hello"));
        if epoch < st.epoch {
            self.stats.stale_epoch += 1;
            return Accept::Stale;
        }
        if seq <= resume {
            self.stats.duplicates += 1;
            return Accept::Ack(resume);
        }
        if seq != resume + 1 {
            self.stats.gaps += 1;
            return Accept::Gap(resume);
        }
        st.last_seq = seq;
        st.high_window = Some(st.high_window.map_or(summary.window, |h| h.max(summary.window)));
        if self.next_window.is_some_and(|n| summary.window < n) {
            self.stats.late += 1;
        } else {
            let w = summary.window;
            match self.pending.get_mut(&w) {
                Some(p) => p.merge(&summary),
                None => {
                    self.pending.insert(w, summary);
                }
            }
            self.next_window.get_or_insert(w);
            self.stats.accepted += 1;
        }
        Accept::Ack(seq)
    }

    /// Releases every window that is complete, in order. A window is
    /// complete when all live edges have reported it or later windows, or
    /// when an edge has reported a window `deadline` later (event time).
    pub fn poll(&mut self, now: i64) -> Vec<WindowSummary> {
        let mut out = Vec::new();
        let Some(mut next) = self.next_window else { return out };
        if self.started_at.is_some_and(|t| now < t.saturating_add(self.grace)) {
            return out;
        }
        let live_watermark = self
            .edges
            .values()
            .filter(|e| now - e.last_seen <= self.liveness)
            .map(|e| e.high_window.unwrap_or(i64::MIN))
            .min();
        // The newest data any edge has reported: event time's frontier.
        let frontier = self.edges.values().filter_map(|e| e.high_window).max();
        let lag = self.deadline.div_euclid(self.resolution).max(1);
        loop {
            let reported = live_watermark.is_some_and(|w| w >= next);
            let expired = frontier.is_some_and(|f| f.saturating_sub(next) >= lag);
            let has_newer = self.pending.keys().next_back().is_some_and(|&k| k >= next);
            if !(reported || (expired && has_newer)) {
                break;
            }
            if !reported {
                self.stats.deadline_releases += 1;
            }
            out.push(self.pending.remove(&next).unwrap_or_else(|| WindowSummary::empty(next)));
            next += 1;
        }
        self.next_window = Some(next);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use etio_core::rng::Rng;

    const RES: i64 = 1_000_000_000;

    fn summary(window: i64, svc: &str, requests: u64) -> WindowSummary {
        let mut s = WindowSummary::empty(window);
        if requests > 0 {
            let st = s.services.entry(svc.to_owned()).or_default();
            st.requests = requests;
            #[allow(clippy::cast_precision_loss)]
            st.duration.add_n(1e6 * (window % 7 + 1) as f64, requests);
        }
        s
    }

    #[test]
    fn outbox_retains_until_every_core_acknowledges() {
        let mut o = Outbox::new("edge-a", 1, 2, 10);
        for w in 0..5 {
            o.push(summary(w, "a", 1));
        }
        o.ack(0, 3);
        assert_eq!(o.len(), 5, "core 1 has acknowledged nothing");
        assert_eq!(o.unacked(0).map(|e| e.seq).collect::<Vec<_>>(), vec![4, 5]);
        o.ack(1, 2);
        assert_eq!(o.len(), 3);
        o.ack(1, 99);
        o.ack(0, 99);
        assert!(o.is_empty(), "acks beyond the last sequence are clamped");
    }

    #[test]
    fn outbox_is_bounded() {
        let mut o = Outbox::new("e", 1, 1, 3);
        for w in 0..5 {
            o.push(summary(w, "a", 1));
        }
        assert_eq!(o.len(), 3);
        assert_eq!(o.stats().dropped, 2);
        assert_eq!(o.unacked(0).next().map(|e| e.seq), Some(3));
    }

    #[test]
    fn collector_deduplicates_and_orders() {
        let mut c = Collector::new(RES, 5 * RES, 30 * RES);
        assert_eq!(c.accept("a", 1, 1, summary(0, "x", 2), 0), Accept::Ack(1));
        assert_eq!(c.accept("a", 1, 1, summary(0, "x", 2), 0), Accept::Ack(1), "retransmission");
        assert_eq!(c.accept("a", 1, 3, summary(2, "x", 2), 0), Accept::Gap(1), "gap");
        assert_eq!(c.accept("a", 1, 2, summary(1, "x", 2), 0), Accept::Ack(2));
        assert!(c.poll(RES).is_empty(), "start-up grace period");
        let released = c.poll(6 * RES);
        assert_eq!(released.iter().map(|s| s.window).collect::<Vec<_>>(), vec![0, 1]);
        assert_eq!(released[0].services["x"].requests, 2, "merged once");
        assert_eq!(c.stats().duplicates, 1);
        assert_eq!(c.stats().gaps, 1);
    }

    #[test]
    fn windows_wait_for_every_live_edge_then_for_the_deadline() {
        let mut c = Collector::new(RES, 5 * RES, 30 * RES);
        c.accept("a", 1, 1, summary(0, "x", 1), 0);
        c.accept("b", 1, 1, summary(0, "y", 1), 0);
        c.accept("a", 1, 2, summary(1, "x", 1), RES);
        // b has not reported window 1 yet.
        assert_eq!(c.poll(5 * RES).len(), 1);
        c.accept("b", 1, 2, summary(1, "y", 1), 5 * RES);
        let w1 = c.poll(5 * RES);
        assert_eq!(w1.len(), 1);
        assert_eq!(w1[0].services.len(), 2, "both edges merged into window 1");
        // b goes silent; a keeps reporting. Window 2 closes once a is a
        // deadline (5 windows of event time) ahead, however long that takes.
        for (seq, w) in (3..).zip(2..7) {
            c.accept("a", 1, seq, summary(w, "x", 1), 6 * RES);
        }
        assert!(c.poll(20 * RES).is_empty(), "a is only 4 windows ahead");
        c.accept("a", 1, 8, summary(7, "x", 1), 6 * RES);
        assert_eq!(c.poll(6 * RES).iter().map(|s| s.window).collect::<Vec<_>>(), vec![2]);
        assert_eq!(c.stats().deadline_releases, 1);
        // Data arriving after its window was released is late.
        c.accept("b", 1, 3, summary(2, "y", 1), 9 * RES);
        assert_eq!(c.stats().late, 1);
        // Once b has been silent for longer than the liveness, it no longer
        // holds anything back.
        c.accept("a", 1, 9, summary(8, "x", 1), 40 * RES);
        assert_eq!(c.poll(40 * RES).iter().map(|s| s.window).collect::<Vec<_>>(), vec![3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn a_restarted_edge_starts_a_new_epoch() {
        let mut c = Collector::new(RES, 5 * RES, 30 * RES);
        c.accept("a", 1, 1, summary(0, "x", 1), 0);
        c.accept("a", 1, 2, summary(1, "x", 1), 0);
        assert_eq!(c.hello("a", 2, 1, 0), 0, "new epoch resumes at sequence 0");
        assert_eq!(c.accept("a", 2, 1, summary(2, "x", 1), 0), Accept::Ack(1));
        assert_eq!(c.accept("a", 1, 3, summary(3, "x", 1), 0), Accept::Stale);
    }

    #[test]
    fn a_restarted_core_adopts_the_edges_position() {
        let mut o = Outbox::new("a", 1, 1, 10);
        for w in 0..5 {
            o.push(summary(w, "x", 1));
        }
        o.ack(0, 3);
        // The core restarted and knows nothing about the edge.
        let mut c = Collector::new(RES, 5 * RES, 30 * RES);
        assert_eq!(c.hello("a", 1, o.first_unacked(0), 0), 3);
        assert_eq!(c.accept("a", 1, 4, summary(3, "x", 1), 0), Accept::Ack(4));
        assert_eq!(c.stats().lost, 0, "acknowledged by the previous incarnation, not lost");
    }

    #[test]
    fn an_overflowing_edge_declares_its_losses() {
        let mut c = Collector::new(RES, 5 * RES, 30 * RES);
        assert_eq!(c.hello("a", 1, 1, 0), 0);
        c.accept("a", 1, 1, summary(0, "x", 1), 0);
        let mut o = Outbox::new("a", 1, 1, 2);
        for w in 0..6 {
            o.push(summary(w, "x", 1));
        }
        o.ack(0, 1);
        assert_eq!(o.first_unacked(0), 5, "sequences 2..=4 were dropped");
        assert_eq!(c.accept("a", 1, 5, summary(4, "x", 1), 0), Accept::Gap(1));
        assert_eq!(c.hello("a", 1, o.first_unacked(0), 0), 4);
        assert_eq!(c.accept("a", 1, 5, summary(4, "x", 1), 0), Accept::Ack(5));
        assert_eq!(c.stats().lost, 3);
        // A stale hello cannot move the position backwards.
        assert_eq!(c.hello("a", 1, 2, 0), 5);
    }

    /// Deterministic simulation: several edges, two cores, a network that
    /// drops, duplicates and delays messages and resets connections. With
    /// delays bounded below the deadline, both cores must end up with exactly
    /// the summaries a single node would have seen, merged once.
    #[test]
    fn simulated_cluster_converges_to_the_single_node_result() {
        for seed in 0..200 {
            run_simulation(seed);
        }
    }

    #[allow(clippy::too_many_lines)]
    fn run_simulation(seed: u64) {
        let mut rng = Rng::seed_from_u64(seed);
        let edges = 1 + rng.index(4);
        let cores = 2;
        let windows = 40i64;
        // Ground truth: what each edge observed in each window.
        let truth: Vec<Vec<WindowSummary>> = (0..edges)
            .map(|e| {
                (0..windows)
                    .map(|w| summary(w, &format!("svc{}", (e + usize::try_from(w).unwrap_or(0)) % 3), rng.below(5)))
                    .collect()
            })
            .collect();
        let mut expected: BTreeMap<i64, WindowSummary> = BTreeMap::new();
        for per_edge in &truth {
            for s in per_edge {
                expected.entry(s.window).or_insert_with(|| WindowSummary::empty(s.window)).merge(s);
            }
        }

        let mut outboxes: Vec<Outbox> = (0..edges).map(|e| Outbox::new(format!("edge-{e}"), 1, cores, 1_000)).collect();
        let mut collectors: Vec<Collector> = (0..cores).map(|_| Collector::new(RES, 20 * RES, 60 * RES)).collect();
        let mut applied: Vec<Vec<WindowSummary>> = vec![Vec::new(); cores];
        // In-flight messages: (deliver_at, edge, core, seq, summary) and acks (deliver_at, edge, core, ack).
        let mut data: Vec<(i64, usize, usize, u64, WindowSummary)> = Vec::new();
        // Acks carry whether the core reported a gap: the transport then
        // rewinds and replays from the acknowledged sequence.
        let mut acks: Vec<(i64, usize, usize, u64, bool)> = Vec::new();
        // Next sequence each edge will transmit to each core, and when the
        // acknowledgements last made progress (the retransmission timer).
        let mut cursor = vec![vec![1u64; cores]; edges];
        let mut progress = vec![vec![0i64; cores]; edges];
        let mut last_ack = vec![vec![0u64; cores]; edges];

        let tick = RES / 4;
        let mut now = 0i64;
        let end = (windows + 40) * RES;
        while now < end {
            // Edges seal one window per second (with empty heartbeats).
            if now % RES == 0 {
                let w = now / RES;
                if w < windows {
                    for (e, o) in outboxes.iter_mut().enumerate() {
                        o.push(truth[e][usize::try_from(w).unwrap_or(0)].clone());
                    }
                }
            }
            // Transmit: each edge sends a few unacknowledged entries per tick;
            // connection resets rewind the cursor to the last acknowledgement.
            for (e, o) in outboxes.iter().enumerate() {
                for core in 0..cores {
                    // Connection resets, and the retransmission timer: without
                    // ack progress for two seconds, replay from the last ack.
                    let stalled = o.unacked(core).next().is_some() && now - progress[e][core] > 2 * RES;
                    if rng.chance(0.02) || stalled {
                        cursor[e][core] = o.acked(core) + 1;
                        progress[e][core] = now;
                    }
                    let batch: Vec<Entry> =
                        o.unacked(core).filter(|x| x.seq >= cursor[e][core]).take(3).cloned().collect();
                    for entry in batch {
                        cursor[e][core] = entry.seq + 1;
                        if rng.chance(0.1) {
                            continue; // dropped
                        }
                        let copies = if rng.chance(0.1) { 2 } else { 1 };
                        for _ in 0..copies {
                            let delay = i64::try_from(rng.below(u64::try_from(3 * RES).unwrap_or(1))).unwrap_or(0);
                            data.push((now + delay, e, core, entry.seq, entry.summary.clone()));
                        }
                    }
                }
            }
            // Deliver due messages in delivery order (ties broken deterministically).
            data.sort_by_key(|m| (m.0, m.1, m.2, m.3));
            let due: Vec<_> = data.iter().take_while(|m| m.0 <= now).cloned().collect();
            data.drain(..due.len());
            for (_, e, core, seq, s) in due {
                let (ack, gap) = match collectors[core].accept(&format!("edge-{e}"), 1, seq, s, now) {
                    Accept::Ack(a) => (a, false),
                    Accept::Gap(a) => (a, true),
                    Accept::Stale => continue,
                };
                if !rng.chance(0.1) {
                    acks.push((
                        now + i64::try_from(rng.below(u64::try_from(RES).unwrap_or(1))).unwrap_or(0),
                        e,
                        core,
                        ack,
                        gap,
                    ));
                }
            }
            acks.sort_by_key(|a| (a.0, a.1, a.2, a.3, a.4));
            let due: Vec<_> = acks.iter().take_while(|a| a.0 <= now).copied().collect();
            acks.drain(..due.len());
            for (_, e, core, a, gap) in due {
                outboxes[e].ack(core, a);
                if a > last_ack[e][core] {
                    last_ack[e][core] = a;
                    progress[e][core] = now;
                }
                if gap || a >= cursor[e][core] {
                    cursor[e][core] = a + 1;
                }
            }
            for (core, c) in collectors.iter_mut().enumerate() {
                applied[core].extend(c.poll(now));
            }
            now += tick;
        }

        for core in 0..cores {
            let got: BTreeMap<i64, WindowSummary> =
                applied[core].iter().filter(|s| s.window < windows).map(|s| (s.window, s.clone())).collect();
            assert_eq!(
                got.len(),
                usize::try_from(windows).unwrap_or(0),
                "seed {seed}: core {core} released every window"
            );
            for (w, s) in &expected {
                assert_eq!(&got[w], s, "seed {seed}: core {core} window {w}");
            }
            let windows_seen: Vec<i64> = applied[core].iter().map(|s| s.window).collect();
            assert!(windows_seen.windows(2).all(|p| p[1] == p[0] + 1), "seed {seed}: released in order, no gaps");
            assert_eq!(collectors[core].stats().late, 0, "seed {seed}: delays stayed below the deadline");
        }
        assert_eq!(applied[0], applied[1], "seed {seed}: the cores agree");
    }
}
