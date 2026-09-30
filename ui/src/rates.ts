// Ingest rates derived from successive status polls (pure, unit-tested).
// The API reports cumulative counters; a rate is the difference between two
// samples over the wall time between them.

import type { Status } from "./api";

export interface Sample {
  /** Wall time of the poll, milliseconds. */
  at: number;
  counters: Record<string, number>;
}

/** The counters the UI tracks, flattened from a status response. */
export function counters(s: Status): Record<string, number> {
  return {
    spans: s.stats.spans,
    traces: s.stats.traces,
    logs: s.stats.logs,
    metric_points: s.stats.metric_points,
    windows: s.stats.windows,
    analyses: s.stats.analyses,
    skew_adjusted: s.stats.skew_adjusted,
    metric_points_unpaired: s.stats.metric_points_unpaired,
    analyses_skipped: s.stats.analyses_skipped,
    late: s.stats.aggregator.late,
    future: s.stats.aggregator.future,
    evicted: s.stats.assembler?.evicted ?? 0,
    oversized: s.stats.assembler?.oversized ?? 0,
  };
}

/** Appends a sample, keeping at most `max`; a counter reset restarts the history. */
export function push(history: readonly Sample[], sample: Sample, max: number): Sample[] {
  const last = history[history.length - 1];
  const reset = last !== undefined && (last.counters.windows ?? 0) > (sample.counters.windows ?? 0);
  const next = reset ? [sample] : [...history, sample];
  return next.slice(Math.max(0, next.length - max));
}

/** Per-second rate of `key` between consecutive samples (one fewer than samples). */
export function series(history: readonly Sample[], key: string): number[] {
  const out: number[] = [];
  for (let i = 1; i < history.length; i++) {
    const a = history[i - 1]!;
    const b = history[i]!;
    const dt = (b.at - a.at) / 1000;
    const dv = (b.counters[key] ?? 0) - (a.counters[key] ?? 0);
    out.push(dt > 0 && dv >= 0 ? dv / dt : 0);
  }
  return out;
}

/** The latest rate of `key`, or null before two samples exist. */
export function latest(history: readonly Sample[], key: string): number | null {
  const s = series(history.slice(-2), key);
  return s.length ? s[0]! : null;
}
