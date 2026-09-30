//! Analysis of complete traces.
//!
//! A trace is a tree of spans across services. From it the engine extracts
//! what per-service metrics cannot show:
//!
//! * **Local time.** For every *entry* into a service (the first span of that
//!   service on a path), the time spent inside the service, i.e. its
//!   duration minus the time spent waiting on other services. A CPU-starved
//!   service shows high local time; a service waiting on a slow dependency
//!   shows high latency but normal local time. This is the single most
//!   useful signal for telling a root cause from a victim.
//! * **Error origins.** A failing request usually marks every span on its
//!   path as failed. The origin is the failing span none of whose children
//!   failed.
//! * **Dependency edges**, including edges to *virtual* nodes for
//!   dependencies that emit no telemetry (databases, caches, external APIs)
//!   but are named by the client span that called them.
//!
//! Timestamps from different hosts are subject to clock skew, which can make
//! a child span appear to start before its parent. Children from another
//! service that do not fit inside their parent are re-centred inside it
//! (the adjustment Jaeger applies), and the correction propagates to their
//! subtree. Offsets are accumulated top-down, so the adjustment is linear in
//! the size of the trace.

use etio_core::Sym;
use hashbrown::HashMap;
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

use crate::span::{Span, SpanStatus};

/// One entry of a request into a service.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceVisit {
    /// Service entered.
    pub service: Sym,
    /// Operation of the entry span.
    pub operation: Sym,
    /// End of the entry span (after skew adjustment), ns since the epoch.
    pub end: i64,
    /// Duration of the entry span, ns.
    pub duration: i64,
    /// Time spent inside the service, ns.
    pub local: i64,
    /// Whether the entry span failed.
    pub error: bool,
    /// Whether the entry span is the root of the trace.
    pub root: bool,
}

/// One call from a service to a dependency.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdgeObservation {
    /// Calling service.
    pub caller: Sym,
    /// Called service, or the named peer of a dependency without telemetry.
    pub callee: Sym,
    /// End of the call, ns since the epoch.
    pub end: i64,
    /// Duration of the call: the caller's client span when span kinds are
    /// known (so network time counts), otherwise the callee's span, ns.
    pub duration: i64,
    /// Whether the call failed (the client span, or the callee, failed).
    pub error: bool,
    /// Whether no span of the callee answered the call: a datastore without
    /// tracing, or a service that did not respond.
    pub virtual_callee: bool,
}

/// A failing span with no failing children.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorOrigin {
    /// Service of the originating span.
    pub service: Sym,
    /// End of the originating span, ns since the epoch.
    pub end: i64,
}

/// Everything extracted from one trace.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceAnalysis {
    /// Entries into services.
    pub visits: Vec<ServiceVisit>,
    /// Calls between services.
    pub edges: Vec<EdgeObservation>,
    /// Where errors originated.
    pub error_origins: Vec<ErrorOrigin>,
    /// Spans whose timestamps were shifted to correct clock skew.
    pub skew_adjusted: u32,
    /// Spans whose parent is missing from the trace (partial traces).
    pub orphans: u32,
    /// Duplicate span identifiers that were ignored.
    pub duplicates: u32,
}

type Children = SmallVec<[u32; 4]>;

/// Analyses the spans of one trace. The order of `spans` does not matter.
#[must_use]
pub fn analyze(spans: &[Span]) -> TraceAnalysis {
    let mut out = TraceAnalysis::default();
    let n = spans.len();
    if n == 0 {
        return out;
    }

    // Index spans by id, ignoring duplicates.
    let mut index: HashMap<u64, u32> = HashMap::with_capacity(n);
    let mut keep = vec![true; n];
    for (i, s) in spans.iter().enumerate() {
        match index.entry(s.span_id) {
            hashbrown::hash_map::Entry::Occupied(_) => {
                keep[i] = false;
                out.duplicates += 1;
            }
            hashbrown::hash_map::Entry::Vacant(e) => {
                e.insert(idx32(i));
            }
        }
    }

    // Parent links and children lists; spans with a missing parent become roots.
    let mut parent: Vec<Option<u32>> = vec![None; n];
    let mut children: Vec<Children> = vec![Children::new(); n];
    let mut roots: Vec<u32> = Vec::new();
    for (i, s) in spans.iter().enumerate() {
        if !keep[i] {
            continue;
        }
        let me = idx32(i);
        match (s.parent_id != 0).then(|| index.get(&s.parent_id).copied()).flatten() {
            Some(p) if p != me => {
                parent[i] = Some(p);
                children[p as usize].push(me);
            }
            _ => {
                if s.parent_id != 0 {
                    out.orphans += 1;
                }
                roots.push(me);
            }
        }
    }

    // Top-down clock-skew offsets (pre-order; cycles cannot occur because
    // every span is visited at most once).
    let mut offset = vec![0i64; n];
    let mut visited = vec![false; n];
    let mut stack: Vec<u32> = roots.clone();
    for &r in &roots {
        visited[r as usize] = true;
    }
    while let Some(u) = stack.pop() {
        let u = u as usize;
        let p = &spans[u];
        let (ps, pe) = (p.start + offset[u], p.end + offset[u]);
        for &c in &children[u] {
            let c = c as usize;
            if visited[c] {
                continue;
            }
            visited[c] = true;
            let ch = &spans[c];
            let (cs, ce) = (ch.start + offset[u], ch.end + offset[u]);
            let mut extra = 0;
            if ch.service != p.service && (cs < ps || ce > pe) && ch.duration() <= p.duration() {
                extra = ps + (p.duration() - ch.duration()) / 2 - cs;
                out.skew_adjusted += 1;
            }
            offset[c] = offset[u] + extra;
            stack.push(idx32(c));
        }
    }
    let start = |i: usize| spans[i].start + offset[i];
    let end = |i: usize| spans[i].end + offset[i];

    // Entry spans: roots, or spans whose parent belongs to another service.
    let mut intervals: Vec<(i64, i64)> = Vec::new();
    let mut walk: Vec<u32> = Vec::new();
    for i in 0..n {
        if !keep[i] || !visited[i] {
            continue;
        }
        let s = &spans[i];
        let is_entry = parent[i].is_none_or(|p| spans[p as usize].service != s.service);
        if !is_entry {
            continue;
        }
        // Walk the service-local subtree; record the intervals spent in other
        // services and the edges leaving the service.
        intervals.clear();
        walk.clear();
        walk.push(idx32(i));
        while let Some(u) = walk.pop() {
            let u = u as usize;
            let us = &spans[u];
            let outbound = us.kind.is_outbound();
            // For an outbound client span, the call is the client span itself:
            // its duration is what the caller waited (network included) and it
            // fails if the callee failed *or never answered*.
            let mut remote_child: Option<usize> = None;
            let mut remote_error = false;
            for &c in &children[u] {
                let c = c as usize;
                let cs = &spans[c];
                if cs.service == s.service {
                    walk.push(idx32(c));
                    continue;
                }
                if outbound {
                    remote_child.get_or_insert(c);
                    remote_error |= cs.is_error();
                } else {
                    // Without span kinds every cross-service child is a call,
                    // measured by the callee's span.
                    intervals.push((start(c), end(c)));
                    out.edges.push(EdgeObservation {
                        caller: s.service,
                        callee: cs.service,
                        end: end(c),
                        duration: cs.duration(),
                        error: cs.is_error(),
                        virtual_callee: false,
                    });
                }
            }
            if outbound {
                // The callee is the service that answered, or the peer the
                // client span names when nothing answered (a datastore without
                // tracing, or a service that is down).
                let callee = remote_child.map(|c| spans[c].service).or((us.peer != Sym::EMPTY).then_some(us.peer));
                if let Some(callee) = callee {
                    intervals.push((start(u), end(u)));
                    out.edges.push(EdgeObservation {
                        caller: s.service,
                        callee,
                        end: end(u),
                        duration: us.duration(),
                        error: us.is_error() || remote_error,
                        virtual_callee: remote_child.is_none(),
                    });
                }
            }
        }
        let (es, ee) = (start(i), end(i));
        let waiting = union_within(&mut intervals, es, ee);
        out.visits.push(ServiceVisit {
            service: s.service,
            operation: s.operation,
            end: ee,
            duration: s.duration(),
            local: (s.duration() - waiting).max(0),
            error: s.is_error(),
            root: parent[i].is_none(),
        });
    }

    // Error origins: failing spans without failing children. A failing
    // client span that nothing answered (no span of another service below
    // it) points at the peer it names: the call failed because the callee
    // did not respond, not because of the caller.
    for i in 0..n {
        let si = &spans[i];
        if keep[i]
            && si.status == SpanStatus::Error
            && !children[i].iter().any(|&c| spans[c as usize].status == SpanStatus::Error)
        {
            let unanswered = si.kind.is_outbound()
                && si.peer != Sym::EMPTY
                && !children[i].iter().any(|&c| spans[c as usize].service != si.service);
            let service = if unanswered { si.peer } else { si.service };
            out.error_origins.push(ErrorOrigin { service, end: end(i) });
        }
    }
    out
}

/// Total length of the union of `intervals`, clipped to `[lo, hi]`.
fn union_within(intervals: &mut [(i64, i64)], lo: i64, hi: i64) -> i64 {
    intervals.sort_unstable();
    let mut total = 0i64;
    let mut cur: Option<(i64, i64)> = None;
    for &(a, b) in intervals.iter() {
        let (a, b) = (a.max(lo), b.min(hi));
        if b <= a {
            continue;
        }
        match cur {
            Some((ca, cb)) if a <= cb => cur = Some((ca, cb.max(b))),
            Some((ca, cb)) => {
                total += cb - ca;
                cur = Some((a, b));
            }
            None => cur = Some((a, b)),
        }
    }
    if let Some((a, b)) = cur {
        total += b - a;
    }
    total
}

#[allow(clippy::cast_possible_truncation)]
const fn idx32(i: usize) -> u32 {
    i as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::SpanKind;

    const MS: i64 = 1_000_000;

    #[allow(clippy::too_many_arguments)]
    fn span(id: u64, parent: u64, svc: u32, kind: SpanKind, start_ms: i64, end_ms: i64, err: bool) -> Span {
        Span {
            trace_id: 1,
            span_id: id,
            parent_id: parent,
            service: Sym(svc),
            operation: Sym(100 + svc),
            kind,
            start: start_ms * MS,
            end: end_ms * MS,
            status: if err { SpanStatus::Error } else { SpanStatus::Unset },
            peer: Sym::EMPTY,
        }
    }

    // Services: 10 = frontend, 11 = cart, 12 = redis-client side.
    fn visit(a: &TraceAnalysis, svc: u32) -> ServiceVisit {
        *a.visits.iter().find(|v| v.service == Sym(svc)).unwrap()
    }

    #[test]
    fn local_time_excludes_downstream_calls() {
        // frontend server 0..100ms, calls cart via a client span 10..70,
        // cart server 15..65 which spends 20..40 in an internal span.
        let spans = [
            span(1, 0, 10, SpanKind::Server, 0, 100, false),
            span(2, 1, 10, SpanKind::Client, 10, 70, false),
            span(3, 2, 11, SpanKind::Server, 15, 65, false),
            span(4, 3, 11, SpanKind::Internal, 20, 40, false),
        ];
        let a = analyze(&spans);
        assert_eq!(a.visits.len(), 2);
        let fe = visit(&a, 10);
        assert!(fe.root);
        // 100ms minus the 60ms client span (network included).
        assert_eq!(fe.local, 40 * MS);
        let cart = visit(&a, 11);
        assert!(!cart.root);
        assert_eq!(cart.local, 50 * MS, "internal spans are local work");
        assert_eq!(a.edges.len(), 1);
        assert_eq!((a.edges[0].caller, a.edges[0].callee), (Sym(10), Sym(11)));
        assert_eq!(a.skew_adjusted, 0);
    }

    #[test]
    fn parallel_calls_are_not_double_counted() {
        let spans = [
            span(1, 0, 10, SpanKind::Unspecified, 0, 100, false),
            span(2, 1, 11, SpanKind::Unspecified, 10, 60, false),
            span(3, 1, 12, SpanKind::Unspecified, 30, 80, false),
        ];
        let a = analyze(&spans);
        // Union of [10,60] and [30,80] is 70ms.
        assert_eq!(visit(&a, 10).local, 30 * MS);
        assert_eq!(a.edges.len(), 2);
    }

    #[test]
    fn clock_skew_is_corrected_for_remote_children() {
        // cart's clock is 1s behind: its span appears to start before its parent.
        let spans = [
            span(1, 0, 10, SpanKind::Server, 1_000, 1_100, false),
            span(2, 1, 11, SpanKind::Server, 10, 70, false),
            span(3, 2, 11, SpanKind::Internal, 20, 30, false),
        ];
        let a = analyze(&spans);
        assert_eq!(a.skew_adjusted, 1);
        // Centred inside the parent: 60ms of the 100ms are spent in cart.
        assert_eq!(visit(&a, 10).local, 40 * MS);
        // The subtree moved with its root.
        assert_eq!(visit(&a, 11).end, (1_000 + 20 + 60) * MS);
    }

    #[test]
    fn error_origin_is_the_deepest_failure() {
        let spans = [
            span(1, 0, 10, SpanKind::Server, 0, 100, true),
            span(2, 1, 11, SpanKind::Server, 10, 90, true),
            span(3, 2, 12, SpanKind::Server, 20, 30, false),
            span(4, 2, 11, SpanKind::Internal, 40, 50, true),
        ];
        let a = analyze(&spans);
        assert_eq!(a.error_origins.len(), 1);
        assert_eq!(a.error_origins[0].service, Sym(11));
    }

    #[test]
    fn virtual_peers_become_edges() {
        let mut client = span(2, 1, 11, SpanKind::Client, 10, 60, false);
        client.peer = Sym(99);
        let spans = [span(1, 0, 11, SpanKind::Server, 0, 100, false), client];
        let a = analyze(&spans);
        assert_eq!(a.edges.len(), 1);
        assert!(a.edges[0].virtual_callee);
        assert_eq!(a.edges[0].callee, Sym(99));
        assert_eq!(visit(&a, 11).local, 50 * MS);
    }

    #[test]
    fn unanswered_calls_are_attributed_to_the_named_peer() {
        // payment is down: the client span fails and no server span exists.
        let mut client = span(2, 1, 10, SpanKind::Client, 10, 12, true);
        client.peer = Sym(11);
        let spans = [span(1, 0, 10, SpanKind::Server, 0, 20, true), client];
        let a = analyze(&spans);
        assert_eq!(a.edges.len(), 1);
        let e = a.edges[0];
        assert_eq!((e.callee, e.error, e.virtual_callee), (Sym(11), true, true));
        assert_eq!(e.duration, 2 * MS);
        assert_eq!(a.error_origins[0].service, Sym(11), "the unresponsive peer is the origin");
    }

    #[test]
    fn client_side_duration_includes_the_network() {
        let spans = [
            span(1, 0, 10, SpanKind::Server, 0, 100, false),
            span(2, 1, 10, SpanKind::Client, 10, 90, false),
            span(3, 2, 11, SpanKind::Server, 30, 70, false),
        ];
        let a = analyze(&spans);
        assert_eq!(a.edges[0].duration, 80 * MS, "the client waited 80 ms, the server worked 40 ms");
    }

    #[test]
    fn partial_traces_and_duplicates_are_tolerated() {
        let spans = [
            span(2, 1, 11, SpanKind::Server, 10, 60, false), // parent 1 missing
            span(3, 2, 12, SpanKind::Server, 20, 30, false),
            span(3, 2, 12, SpanKind::Server, 20, 30, false), // duplicate
        ];
        let a = analyze(&spans);
        assert_eq!(a.orphans, 1);
        assert_eq!(a.duplicates, 1);
        assert_eq!(a.visits.len(), 2);
        assert!(visit(&a, 11).root);
    }

    #[test]
    fn self_parent_and_empty_traces() {
        assert_eq!(analyze(&[]), TraceAnalysis::default());
        let s = span(1, 1, 10, SpanKind::Server, 0, 10, false);
        let a = analyze(&[s]);
        assert_eq!(a.visits.len(), 1);
    }

    #[test]
    fn union_within_clips_and_merges() {
        let mut v = vec![(0, 10), (5, 15), (20, 30), (-5, 2)];
        assert_eq!(union_within(&mut v, 0, 25), 15 + 5);
        assert_eq!(union_within(&mut [], 0, 10), 0);
    }
}
