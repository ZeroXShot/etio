// Typed client of the Etio REST API (`/api/v1`). Types mirror the Rust
// structures serialised by the server; times are nanoseconds since the epoch
// unless the field name says otherwise.

export type Role = "standalone" | "edge" | "core";
export type IncidentStatus = "open" | "resolved";

export interface Status {
  version: string;
  role: Role;
  now: number;
  resolution_s: number;
  lateness_s: number;
  series: number;
  stats: {
    spans: number;
    traces: number;
    logs: number;
    metric_points: number;
    windows: number;
    analyses: number;
  };
}

export interface IncidentSummary {
  id: string;
  status: IncidentStatus;
  start: number;
  opened_at: number;
  resolved_at: number | null;
  services: number;
  top: [string, number] | null;
  analyses: number;
}

export interface SeriesScore {
  max: number;
  sustained: number;
  shift: number;
  onset_s: number | null;
  reference_median: number;
  scale: number;
  peak_value: number;
}

export interface Signal {
  service: string;
  name: string;
  category: string;
  score: SeriesScore;
}

export interface RankedService {
  rank: number;
  service: string;
  score: number;
  probability: number;
  contributions: [string, number][];
  features: Record<string, number>;
  signals: Signal[];
  reasons: string[];
}

export interface RcaResult {
  method: string;
  model: string | null;
  /** Seconds since the epoch. */
  anomaly_time: number;
  /** Seconds since the epoch. */
  reference: [number, number];
  /** Seconds since the epoch. */
  abnormal: [number, number];
  ranking: RankedService[];
  skipped_series: number;
  warnings: string[];
}

export interface Incident {
  id: string;
  status: IncidentStatus;
  start: number;
  opened_at: number;
  resolved_at: number | null;
  last_anomaly_at: number;
  trigger: string[];
  /** The most surprising series of each triggering service. */
  trigger_signals?: Record<string, string>;
  services: Record<string, number>;
  rca: RcaResult | null;
  analyses: number;
}

export interface ServiceView {
  service: string;
  series: number;
  anomalous: string[];
}

export interface Graph {
  nodes: string[];
  edges: [string, string, number][];
}

export interface SeriesMeta {
  service: string;
  name: string;
  category: string;
  direction: string;
  created: number;
}

export interface Values {
  /** Window start times, seconds since the epoch. */
  times: number[];
  values: (number | null)[];
}

export type EngineEvent =
  | { type: "incident_opened"; incident: Incident }
  | { type: "incident_analyzed"; incident: Incident }
  | { type: "incident_resolved"; incident: Incident };

export class ApiError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
  }
}

const TOKEN_KEY = "etio.token";

/** The API read token, kept for the browser session only. */
export const token = {
  get(): string | null {
    try {
      return sessionStorage.getItem(TOKEN_KEY);
    } catch {
      return null;
    }
  },
  set(value: string | null): void {
    try {
      if (value) sessionStorage.setItem(TOKEN_KEY, value);
      else sessionStorage.removeItem(TOKEN_KEY);
    } catch {
      // Storage unavailable (private mode): the token lasts for this page only.
    }
  },
};

export function authHeaders(): Record<string, string> {
  const t = token.get();
  return t ? { Authorization: `Bearer ${t}` } : {};
}

async function request<T>(path: string, init: RequestInit = {}): Promise<T> {
  const res = await fetch(`/api/v1${path}`, {
    ...init,
    headers: { Accept: "application/json", ...authHeaders(), ...(init.headers ?? {}) },
  });
  if (!res.ok) {
    let message = res.statusText;
    try {
      const body = (await res.json()) as { error?: string };
      message = body.error ?? message;
    } catch {
      // Not JSON.
    }
    throw new ApiError(res.status, message);
  }
  if (res.status === 204) return undefined as T;
  return (await res.json()) as T;
}

const q = encodeURIComponent;

export const api = {
  status: () => request<Status>("/status"),
  incidents: (limit = 100) => request<IncidentSummary[]>(`/incidents?limit=${limit}`),
  incident: (id: string) => request<Incident>(`/incidents/${q(id)}`),
  feedback: (id: string, rootCause: string, comment: string) =>
    request<void>(`/incidents/${q(id)}/feedback`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ root_cause: rootCause, comment }),
    }),
  services: () => request<ServiceView[]>("/services"),
  graph: () => request<Graph>("/graph"),
  series: (service?: string) => request<SeriesMeta[]>(service ? `/series?service=${q(service)}` : "/series"),
  values: (service: string, name: string, from?: number, to?: number) => {
    let path = `/series/values?service=${q(service)}&name=${q(name)}`;
    if (from !== undefined) path += `&from=${from}`;
    if (to !== undefined) path += `&to=${to}`;
    return request<Values>(path);
  },
  analyze: (anomalyTime: number, method?: string) =>
    request<RcaResult>("/analyze", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(method ? { anomaly_time: anomalyTime, method } : { anomaly_time: anomalyTime }),
    }),
};
