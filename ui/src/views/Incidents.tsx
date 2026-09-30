import { useMemo, useState } from "preact/hooks";
import type { IncidentSummary } from "../api";
import { Meter, StatusMark } from "../components/Meter";
import { Empty, Panel, Readout } from "../components/Panel";
import { IncidentHistory } from "../components/Timeline";
import { ago, clock, day, formatDuration, offset, probability } from "../format";
import { useKeys } from "../hooks";
import { usePrefs } from "../prefs";
import "./Incidents.css";

type Filter = "all" | "open" | "resolved";

const S = 1e9;

function median(xs: number[]): number | null {
  if (xs.length === 0) return null;
  const s = [...xs].sort((a, b) => a - b);
  const m = Math.floor(s.length / 2);
  return s.length % 2 ? s[m]! : (s[m - 1]! + s[m]!) / 2;
}

function go(id: string) {
  location.hash = `#/incidents/${encodeURIComponent(id)}`;
}

export function Incidents({
  incidents,
  now,
  hasTelemetry,
}: {
  incidents: IncidentSummary[];
  now: number;
  hasTelemetry: boolean;
}) {
  const { zone } = usePrefs();
  const [filter, setFilter] = useState<Filter>("all");
  const [hovered, setHovered] = useState<string | null>(null);
  const [cursor, setCursor] = useState(-1);
  const rows = useMemo(() => incidents.filter((i) => filter === "all" || i.status === filter), [incidents, filter]);
  useKeys(
    {
      j: () => setCursor((c) => Math.min(rows.length - 1, c + 1)),
      k: () => setCursor((c) => Math.max(0, c - 1)),
      Enter: () => {
        const row = rows[cursor];
        if (row) go(row.id);
      },
    },
    [rows, cursor],
  );

  const open = incidents.filter((i) => i.status === "open").length;
  const detection = median(incidents.map((i) => (i.opened_at - i.start) / S));
  const duration = median(incidents.filter((i) => i.resolved_at !== null).map((i) => (i.resolved_at! - i.start) / S));
  const today = day(now, zone);

  return (
    <>
      <div class="page-head">
        <div class="page-title">
          <h1>Incidents</h1>
          <p class="page-sub">
            Opened when several services turn anomalous together, one turns severely anomalous, or an entry point's
            latency, errors or traffic deviate; the candidate root causes are re-ranked as evidence accumulates.
          </p>
        </div>
        <div class="page-readouts">
          <Readout label="Open" tone={open > 0 ? "signal" : undefined}>
            {open}
          </Readout>
          <Readout label="Resolved">{incidents.length - open}</Readout>
          <Readout label="Median detection" note="onset to open">
            {detection === null ? "–" : offset(detection)}
          </Readout>
          <Readout label="Median duration" note="resolved only">
            {duration === null ? "–" : formatDuration(duration)}
          </Readout>
        </div>
      </div>

      {incidents.length === 0 ? (
        <Panel title="Log">
          {hasTelemetry ? (
            <Empty mode="quiet" title="No incidents">
              Telemetry has been received and nothing is anomalous. Detection starts after a warm-up in which each
              series' normal behaviour is learned: about 25 minutes with the default 10 s window.
            </Empty>
          ) : (
            <Empty mode="no-signal" title="No telemetry yet">
              Point an OpenTelemetry SDK or Collector at this server: OTLP over gRPC (port 4317 by default) or HTTP
              (4318). Incidents appear here once baselines are learned.
            </Empty>
          )}
        </Panel>
      ) : (
        <>
          <Panel title="History" meta={`${incidents.length} incident${incidents.length === 1 ? "" : "s"} in memory`}>
            <IncidentHistory incidents={incidents} now={now} hovered={hovered} onHover={setHovered} />
          </Panel>
          <Panel
            title="Log"
            meta={
              <span class="toggle" role="group" aria-label="Filter by status">
                {(["all", "open", "resolved"] as const).map((f) => (
                  <button key={f} type="button" aria-pressed={filter === f} onClick={() => setFilter(f)}>
                    {f === "all" ? "All" : f === "open" ? "Open" : "Resolved"}{" "}
                    <span class="muted">
                      {f === "all" ? incidents.length : incidents.filter((i) => i.status === f).length}
                    </span>
                  </button>
                ))}
              </span>
            }
            flush
          >
            <table class="table incident-log">
              <thead>
                <tr>
                  <th>Status</th>
                  <th>Incident</th>
                  <th>Onset</th>
                  <th class="num">Detected</th>
                  <th class="num">Duration</th>
                  <th>Most likely root cause</th>
                  <th class="num">Services</th>
                  <th class="num">Analyses</th>
                </tr>
              </thead>
              <tbody>
                {rows.map((i, k) => {
                  const d = day(i.start, zone);
                  return (
                    <tr
                      key={i.id}
                      class={`row${hovered === i.id ? " hover" : ""}${cursor === k ? " current" : ""}`}
                      onClick={() => go(i.id)}
                      onMouseEnter={() => setHovered(i.id)}
                      onMouseLeave={() => setHovered(null)}
                    >
                      <td>
                        <StatusMark status={i.status} />
                      </td>
                      <td>
                        <a class="mono incident-id" href={`#/incidents/${encodeURIComponent(i.id)}`}>
                          {i.id}
                        </a>
                      </td>
                      <td>
                        <span class="mono">{clock(i.start, zone)}</span>
                        <span class="cell-note">{d === today ? ago(i.start, now) : d}</span>
                      </td>
                      <td class="num mono">{offset((i.opened_at - i.start) / S)}</td>
                      <td class="num">
                        <span class="mono">{formatDuration(((i.resolved_at ?? now) - i.start) / S)}</span>
                        {i.status === "open" && <span class="cell-note">ongoing</span>}
                      </td>
                      <td>
                        {i.top ? (
                          <span class="suspect">
                            <strong class="mono">{i.top[0]}</strong>
                            <Meter value={i.top[1]} cells={12} tone={i.status === "open" ? "signal" : "ink"} />
                            <span class="mono num">{probability(i.top[1])}</span>
                          </span>
                        ) : (
                          <span class="muted">analysis pending</span>
                        )}
                      </td>
                      <td class="num mono">{i.services}</td>
                      <td class="num mono">{i.analyses}</td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </Panel>
        </>
      )}
    </>
  );
}
