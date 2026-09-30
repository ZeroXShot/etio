// Formatting helpers (pure, unit-tested). Times from the API are
// nanoseconds since the epoch unless a name says otherwise.

import type { Zone } from "./prefs";

const NS_PER_S = 1e9;
/** A true minus sign, the width of a plus in tabular figures. */
const MINUS = "−";

export function nsToDate(ns: number): Date {
  return new Date(ns / 1e6);
}

interface Parts {
  y: number;
  mo: number;
  d: number;
  h: number;
  mi: number;
  s: number;
}

function parts(ns: number, zone: Zone): Parts {
  const t = nsToDate(ns);
  return zone === "utc"
    ? {
        y: t.getUTCFullYear(),
        mo: t.getUTCMonth() + 1,
        d: t.getUTCDate(),
        h: t.getUTCHours(),
        mi: t.getUTCMinutes(),
        s: t.getUTCSeconds(),
      }
    : {
        y: t.getFullYear(),
        mo: t.getMonth() + 1,
        d: t.getDate(),
        h: t.getHours(),
        mi: t.getMinutes(),
        s: t.getSeconds(),
      };
}

const p2 = (n: number) => String(n).padStart(2, "0");

/** "14:03:12". */
export function clock(ns: number, zone: Zone = "local"): string {
  const t = parts(ns, zone);
  return `${p2(t.h)}:${p2(t.mi)}:${p2(t.s)}`;
}

/** "14:03". */
export function clockShort(ns: number, zone: Zone = "local"): string {
  const t = parts(ns, zone);
  return `${p2(t.h)}:${p2(t.mi)}`;
}

/** "2026-09-30". */
export function day(ns: number, zone: Zone = "local"): string {
  const t = parts(ns, zone);
  return `${t.y}-${p2(t.mo)}-${p2(t.d)}`;
}

/** "2026-09-30 14:03:12". */
export function formatTime(ns: number, zone: Zone = "local"): string {
  return `${day(ns, zone)} ${clock(ns, zone)}`;
}

/** The zone's name as an offset from UTC at `ns`, e.g. "UTC+02:00". */
export function zoneLabel(zone: Zone, ns: number): string {
  if (zone === "utc") return "UTC";
  const off = -nsToDate(ns).getTimezoneOffset();
  const a = Math.abs(off);
  return `UTC${off < 0 ? MINUS : "+"}${p2(Math.floor(a / 60))}:${p2(a % 60)}`;
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

/** A signed duration: "+30s", "−5s", "+9m 20s". */
export function offset(seconds: number): string {
  if (!Number.isFinite(seconds)) return "–";
  return `${seconds < 0 ? MINUS : "+"}${formatDuration(Math.abs(seconds))}`;
}

/** "12s ago", relative to `nowNs`. */
export function ago(ns: number, nowNs: number): string {
  return `${formatDuration((nowNs - ns) / NS_PER_S)} ago`;
}

export function percent(p: number, digits = 0): string {
  return `${(p * 100).toFixed(digits)}%`;
}

/** A probability as a whole percentage, "<1%" rather than a misleading "0%". */
export function probability(p: number): string {
  if (!Number.isFinite(p)) return "–";
  if (p > 0 && p < 0.005) return "<1%";
  return percent(p);
}

/** "+1.45", "−2.29". */
export function signed(v: number, digits = 2): string {
  if (!Number.isFinite(v)) return "–";
  const s = Math.abs(v).toFixed(digits);
  if (Number(s) === 0) return s;
  return `${v < 0 ? MINUS : "+"}${s}`;
}

/** Integers with thousands separators: "1,554,186". */
export function count(n: number): string {
  if (!Number.isFinite(n)) return "–";
  const s = String(Math.round(Math.abs(n))).replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  return n < 0 ? `${MINUS}${s}` : s;
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

/** A rate per second, "29.6k/s". */
export function rate(v: number | null): string {
  return v === null ? "–" : `${formatValue(v)}/s`;
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

/** Display names of signal categories, in the order views group them. */
export const CATEGORIES: [string, string][] = [
  ["latency", "Latency"],
  ["errors", "Errors"],
  ["traffic", "Traffic"],
  ["self_time", "Own processing time"],
  ["error_origin", "Error origin"],
  ["cpu", "CPU"],
  ["memory", "Memory"],
  ["disk", "Disk"],
  ["network", "Network"],
  ["connections", "Connections"],
  ["runtime", "Runtime"],
  ["logs", "Logs"],
  ["other", "Other"],
];

export function categoryName(c: string): string {
  return CATEGORIES.find(([k]) => k === c)?.[1] ?? c.replaceAll("_", " ");
}

export function categoryOrder(c: string): number {
  const i = CATEGORIES.findIndex(([k]) => k === c);
  return i < 0 ? CATEGORIES.length : i;
}
