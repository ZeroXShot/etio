// Formatting helpers (pure, unit-tested).

const NS_PER_S = 1e9;

export function nsToDate(ns: number): Date {
  return new Date(ns / 1e6);
}

/** "2026-09-30 14:03:12" in local time. */
export function formatTime(ns: number): string {
  const d = nsToDate(ns);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

/** Compact duration: "45s", "3m 20s", "2h 5m", "3d 4h". */
export function formatDuration(seconds: number): string {
  if (!Number.isFinite(seconds)) return "–";
  const s = Math.max(0, Math.round(seconds));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return s % 60 ? `${m}m ${s % 60}s` : `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return m % 60 ? `${h}h ${m % 60}m` : `${h}h`;
  const d = Math.floor(h / 24);
  return h % 24 ? `${d}d ${h % 24}h` : `${d}d`;
}

export function durationBetween(fromNs: number, toNs: number): string {
  return formatDuration((toNs - fromNs) / NS_PER_S);
}

/** "12 s ago", relative to `nowNs`. */
export function ago(ns: number, nowNs: number): string {
  return `${formatDuration((nowNs - ns) / NS_PER_S)} ago`;
}

export function percent(p: number, digits = 0): string {
  return `${(p * 100).toFixed(digits)}%`;
}

/** Numbers with a sensible number of significant digits and SI suffixes. */
export function formatValue(v: number | null | undefined): string {
  if (v === null || v === undefined || !Number.isFinite(v)) return "–";
  const a = Math.abs(v);
  if (a >= 1e9) return `${(v / 1e9).toPrecision(3)}G`;
  if (a >= 1e6) return `${(v / 1e6).toPrecision(3)}M`;
  if (a >= 1e4) return `${(v / 1e3).toPrecision(3)}k`;
  if (a === 0) return "0";
  if (a < 1e-3) return v.toExponential(1);
  return String(Number(v.toPrecision(3)));
}

/** Human names of model features. */
const FEATURES: Record<string, string> = {
  log_max: "peak anomaly",
  rel_max: "peak relative to the strongest service",
  rank_score: "rank by peak",
  log_sustained: "sustained anomaly",
  log_resource: "resource signals",
  log_latency: "latency signals",
  log_errors: "error signals",
  log_traffic: "traffic signals",
  log_logs: "log signals",
  log_trace_local: "own processing time",
  frac_anomalous: "share of anomalous signals",
  onset_lead: "anomaly started first",
  has_onset: "has an onset",
  walk: "graph random walk",
  callee_explained: "explained by a callee",
  upstream_anomalous: "callers anomalous",
  downstream_anomalous: "callees anomalous",
  is_entry: "entry point",
  in_graph: "in the dependency graph",
  frac_silent: "signals went silent",
};

export function featureName(f: string): string {
  return FEATURES[f] ?? f.replaceAll("_", " ");
}
