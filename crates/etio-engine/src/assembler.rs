//! Streaming trace assembly.
//!
//! Spans of one trace arrive from many services, in many batches, in any
//! order. The assembler buffers them by trace identifier and releases a trace
//! once no span has arrived for it during `timeout` (on the engine clock), at
//! which point it is analysed as a whole.
//!
//! Memory is bounded twice: by the total number of buffered spans (when full,
//! the traces that have waited longest are released early; analysing a
//! partial trace degrades gracefully) and by the number of spans of a single
//! trace (runaway traces are released in chunks).

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use etio_pipeline::Span;
use hashbrown::HashMap;
use serde::{Deserialize, Serialize};

/// Counters of the assembler.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssemblerStats {
    /// Traces released after going idle.
    pub completed: u64,
    /// Traces released early because the span budget was exhausted.
    pub evicted: u64,
    /// Chunks released because a single trace exceeded its span budget.
    pub oversized: u64,
}

struct Buffer {
    spans: Vec<Span>,
    last_arrival: i64,
}

/// Buffers spans until their trace is complete.
pub struct TraceAssembler {
    timeout: i64,
    max_spans: usize,
    max_spans_per_trace: usize,
    buffered: usize,
    traces: HashMap<u128, Buffer>,
    /// Deadlines, validated lazily against `Buffer::last_arrival`.
    deadlines: BinaryHeap<Reverse<(i64, u128)>>,
    stats: AssemblerStats,
}

impl TraceAssembler {
    /// Creates an assembler. `timeout` is in nanoseconds of engine time.
    #[must_use]
    pub fn new(timeout: i64, max_spans: usize, max_spans_per_trace: usize) -> Self {
        Self {
            timeout: timeout.max(0),
            max_spans: max_spans.max(1),
            max_spans_per_trace: max_spans_per_trace.max(1),
            buffered: 0,
            traces: HashMap::new(),
            deadlines: BinaryHeap::new(),
            stats: AssemblerStats::default(),
        }
    }

    /// Counters.
    #[must_use]
    pub const fn stats(&self) -> AssemblerStats {
        self.stats
    }

    /// Spans currently buffered.
    #[must_use]
    pub const fn buffered_spans(&self) -> usize {
        self.buffered
    }

    /// Traces currently buffered.
    #[must_use]
    pub fn buffered_traces(&self) -> usize {
        self.traces.len()
    }

    /// Adds a span that arrived at engine time `now`. Returns traces that had
    /// to be released immediately to respect the memory bounds.
    pub fn add(&mut self, span: Span, now: i64) -> Vec<Vec<Span>> {
        let mut released = Vec::new();
        let id = span.trace_id;
        let buf = self.traces.entry(id).or_insert_with(|| Buffer { spans: Vec::new(), last_arrival: now });
        let fresh = buf.spans.is_empty();
        buf.spans.push(span);
        buf.last_arrival = buf.last_arrival.max(now);
        self.buffered += 1;
        if fresh {
            self.deadlines.push(Reverse((now + self.timeout, id)));
        }
        if buf.spans.len() >= self.max_spans_per_trace {
            let spans = std::mem::take(&mut buf.spans);
            self.buffered -= spans.len();
            self.traces.remove(&id);
            self.stats.oversized += 1;
            released.push(spans);
        }
        while self.buffered > self.max_spans {
            match self.pop_oldest() {
                Some(spans) => {
                    self.stats.evicted += 1;
                    released.push(spans);
                }
                None => break,
            }
        }
        released
    }

    /// Releases the trace with the earliest valid deadline.
    fn pop_oldest(&mut self) -> Option<Vec<Span>> {
        while let Some(Reverse((_, id))) = self.deadlines.pop() {
            if let Some(buf) = self.traces.remove(&id) {
                self.buffered -= buf.spans.len();
                return Some(buf.spans);
            }
        }
        None
    }

    /// Releases every trace that has been idle for at least the timeout.
    pub fn release_idle(&mut self, now: i64) -> Vec<Vec<Span>> {
        let mut out = Vec::new();
        while let Some(&Reverse((deadline, id))) = self.deadlines.peek() {
            if deadline > now {
                break;
            }
            self.deadlines.pop();
            let Some(buf) = self.traces.get(&id) else { continue }; // stale entry
            let due = buf.last_arrival + self.timeout;
            if due > now {
                // New spans arrived since this deadline was set: re-arm.
                self.deadlines.push(Reverse((due, id)));
                continue;
            }
            if let Some(buf) = self.traces.remove(&id) {
                self.buffered -= buf.spans.len();
                self.stats.completed += 1;
                out.push(buf.spans);
            }
        }
        out
    }

    /// Releases everything (shutdown, end of a replay).
    pub fn release_all(&mut self) -> Vec<Vec<Span>> {
        self.deadlines.clear();
        self.buffered = 0;
        let mut out: Vec<(u128, Vec<Span>)> = self.traces.drain().map(|(id, b)| (id, b.spans)).collect();
        // Deterministic order regardless of hash-map iteration order.
        out.sort_unstable_by_key(|(id, _)| *id);
        self.stats.completed += out.len() as u64;
        out.into_iter().map(|(_, s)| s).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use etio_core::Sym;
    use etio_pipeline::{SpanKind, SpanStatus};

    fn span(trace: u128, id: u64) -> Span {
        Span {
            trace_id: trace,
            span_id: id,
            parent_id: 0,
            service: Sym(2),
            operation: Sym(3),
            kind: SpanKind::Server,
            start: 0,
            end: 10,
            status: SpanStatus::Unset,
            peer: Sym::EMPTY,
        }
    }

    #[test]
    fn releases_traces_after_idle_timeout() {
        let mut a = TraceAssembler::new(100, 1_000, 1_000);
        assert!(a.add(span(1, 1), 0).is_empty());
        a.add(span(2, 1), 50);
        a.add(span(1, 2), 80); // trace 1 is active again
        assert!(a.release_idle(99).is_empty());
        // Trace 2 is due at 150; trace 1 is re-armed to 180 by its second span.
        let r = a.release_idle(150);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0][0].trace_id, 2);
        assert!(a.release_idle(179).is_empty());
        let r = a.release_idle(180);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].len(), 2);
        assert_eq!(a.buffered_spans(), 0);
        assert_eq!(a.stats().completed, 2);
    }

    #[test]
    fn span_budget_evicts_oldest_traces() {
        let mut a = TraceAssembler::new(1_000, 3, 100);
        a.add(span(1, 1), 0);
        a.add(span(1, 2), 0);
        a.add(span(2, 1), 10);
        let released = a.add(span(3, 1), 20);
        assert_eq!(released.len(), 1);
        assert_eq!(released[0][0].trace_id, 1);
        assert_eq!(a.buffered_spans(), 2);
        assert_eq!(a.stats().evicted, 1);
    }

    #[test]
    fn oversized_traces_are_chunked() {
        let mut a = TraceAssembler::new(1_000, 100, 2);
        assert!(a.add(span(9, 1), 0).is_empty());
        let r = a.add(span(9, 2), 0);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].len(), 2);
        assert_eq!(a.buffered_traces(), 0);
        assert_eq!(a.stats().oversized, 1);
    }

    #[test]
    fn release_all_is_deterministic() {
        let mut a = TraceAssembler::new(1_000, 100, 100);
        for t in [5u128, 3, 9, 1] {
            a.add(span(t, 1), 0);
        }
        let ids: Vec<u128> = a.release_all().iter().map(|s| s[0].trace_id).collect();
        assert_eq!(ids, vec![1, 3, 5, 9]);
        assert_eq!(a.buffered_spans(), 0);
    }
}
